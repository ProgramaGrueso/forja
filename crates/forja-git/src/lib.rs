use std::path::{Path, PathBuf};
use std::process::Command;

use forja_core::{ForjaError, Result};

/// Crea un worktree de git en `destino` sobre una nueva rama `nueva_rama`
/// creada a partir de `base_branch` dentro del repositorio `repo`.
pub fn create_worktree(
    repo: &Path,
    base_branch: &str,
    nueva_rama: &str,
    destino: &Path,
) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(nueva_rama)
        .arg(destino)
        .arg(base_branch)
        .output()
        .map_err(|err| ForjaError::Git(format!("no se pudo ejecutar git: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = match (stderr.is_empty(), stdout.is_empty()) {
            (false, false) => format!("stderr: {stderr}; stdout: {stdout}"),
            (false, true) => stderr,
            (true, false) => stdout,
            (true, true) => format!("código de salida {:?}", output.status.code()),
        };
        return Err(ForjaError::Git(format!(
            "git worktree add falló para rama '{nueva_rama}' desde '{base_branch}': {detail}"
        )));
    }

    Ok(destino.to_path_buf())
}

fn failure_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match (stderr.is_empty(), stdout.is_empty()) {
        (false, false) => format!("stderr: {stderr}; stdout: {stdout}"),
        (false, true) => stderr,
        (true, false) => stdout,
        (true, true) => format!("código de salida {:?}", output.status.code()),
    }
}

fn run_git_cmd(repo: &Path, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| ForjaError::Git(format!("no se pudo ejecutar git: {err}")))?;

    if !output.status.success() {
        return Err(ForjaError::Git(format!(
            "{context} falló: {}",
            failure_detail(&output)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Recorta `text` a `max_bytes` sin partir un carácter UTF-8.
fn truncate_with(text: String, max_bytes: usize, marker: &str) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = text[..cut].to_string();
    out.push('\n');
    out.push_str(marker);
    out.push('\n');
    (out, true)
}

fn truncate_bytes(text: String, max_bytes: usize) -> (String, bool) {
    truncate_with(text, max_bytes, "…[diff truncado]")
}

/// Diff del worktree contra la rama base (incluye cambios sin commitear).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDiff {
    /// `git diff --stat` (resumen por archivo).
    pub stat: String,
    /// Diff completo sin colores.
    pub text: String,
    /// `true` si `text` se recortó a `max_bytes`.
    pub truncated: bool,
}

/// Tope del resumen por archivo (bytes UTF-8).
const MAX_STAT_BYTES: usize = 64 * 1024;

/// Rutas sin seguimiento del worktree (una entrada por archivo o carpeta).
fn untracked_files(worktree: &Path) -> Result<Vec<String>> {
    let stdout = run_git_cmd(
        worktree,
        &["status", "--porcelain", "-z", "--untracked-files=normal"],
        "git status --porcelain",
    )?;
    Ok(stdout
        .split('\0')
        .filter_map(|entry| entry.strip_prefix("?? ").map(str::to_string))
        .collect())
}

/// Marca rutas con `git add -N` para que `git diff` muestre su contenido.
fn add_intent_to_add(worktree: &Path, paths: &[String]) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(worktree)
        .args(["add", "-N", "--"])
        .args(paths);
    let output = command
        .output()
        .map_err(|err| ForjaError::Git(format!("no se pudo ejecutar git: {err}")))?;
    if !output.status.success() {
        return Err(ForjaError::Git(format!(
            "git add -N falló: {}",
            failure_detail(&output)
        )));
    }
    Ok(())
}

/// Deshace el `add -N` anterior sin tocar el contenido del worktree.
fn reset_intent_to_add(worktree: &Path, paths: &[String]) {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(worktree)
        .args(["reset", "-q", "--"])
        .args(paths);
    let _ = command.output();
}

/// Punto donde la rama del worktree se separó de `base_branch`.
///
/// Diffs contra la punta de la base mostrarían como cambios de la tarea los
/// commits que otros worktrees fusionaron después. Si no hay ancestro común
/// (historiales sin relación) se usa la propia rama base.
fn merge_base(worktree: &Path, base_branch: &str) -> String {
    run_git_cmd(
        worktree,
        &["merge-base", base_branch, "HEAD"],
        "git merge-base",
    )
    .ok()
    .map(|out| out.trim().to_string())
    .filter(|out| !out.is_empty())
    .unwrap_or_else(|| base_branch.to_string())
}

/// Diff del árbol de trabajo de `worktree` contra `base_branch`.
///
/// Se compara con el `merge-base` (lo que hizo la tarea) e incluye los archivos
/// sin seguimiento: se marcan temporalmente con `git add -N` y el índice se
/// restaura al terminar, de modo que el worktree queda exactamente como estaba.
/// El resultado se recorta a `max_bytes`.
pub fn diff_worktree(worktree: &Path, base_branch: &str, max_bytes: usize) -> Result<WorktreeDiff> {
    let untracked = untracked_files(worktree)?;
    if !untracked.is_empty() {
        add_intent_to_add(worktree, &untracked)?;
    }

    let reference = merge_base(worktree, base_branch);
    let result: Result<(String, String)> = (|| {
        let stat = run_git_cmd(
            worktree,
            &["diff", "--no-color", "--stat", &reference],
            "git diff --stat",
        )?;
        let full = run_git_cmd(
            worktree,
            &["diff", "--no-color", "--find-renames", &reference],
            "git diff",
        )?;
        Ok((stat, full))
    })();

    if !untracked.is_empty() {
        reset_intent_to_add(worktree, &untracked);
    }

    let (stat, full) = result?;
    let (stat, stat_truncated) = truncate_with(stat, MAX_STAT_BYTES, "…[resumen truncado]");
    let (text, truncated) = truncate_bytes(full, max_bytes);
    Ok(WorktreeDiff {
        stat,
        text,
        truncated: truncated || stat_truncated,
    })
}

/// Resultado de fusionar una rama de tarea en la rama base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// Hash corto de `HEAD` tras la fusión.
    pub commit: String,
    /// `true` si git no tuvo nada que fusionar.
    pub already_up_to_date: bool,
}

/// Fusiona `branch` en `base_branch` dentro de `repo` con un commit de fusión.
///
/// Exige que el repositorio esté en `base_branch` y sin cambios pendientes. Si
/// hay conflictos, anula la fusión y devuelve [`ForjaError::Conflict`] dejando
/// el árbol como estaba.
pub fn merge_branch(
    repo: &Path,
    base_branch: &str,
    branch: &str,
    message: &str,
) -> Result<MergeOutcome> {
    if branch.trim().is_empty() || branch == base_branch {
        return Err(ForjaError::InvalidInput(
            "la rama a fusionar no puede ser la rama base ni estar vacía".to_string(),
        ));
    }
    ensure_merge_base_ready(repo, base_branch)?;

    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge", "--no-ff", "-m", message, branch])
        .output()
        .map_err(|err| ForjaError::Git(format!("no se pudo ejecutar git: {err}")))?;

    if !output.status.success() {
        // No dejar el repo a medio fusionar: el siguiente intento debe partir limpio.
        let _ = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["merge", "--abort"])
            .output();
        return Err(ForjaError::Conflict(format!(
            "git merge falló (se anuló la fusión): {}",
            failure_detail(&output)
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
    let already_up_to_date =
        stdout.contains("already up to date") || stdout.contains("ya está actualizado");
    let commit = run_git_cmd(
        repo,
        &["rev-parse", "--short", "HEAD"],
        "git rev-parse --short HEAD",
    )?;
    Ok(MergeOutcome {
        commit: commit.trim().to_string(),
        already_up_to_date,
    })
}

/// Comprueba que `repo` está en `base_branch` y sin cambios pendientes, es decir,
/// listo para recibir una fusión. No modifica nada.
pub fn ensure_merge_base_ready(repo: &Path, base_branch: &str) -> Result<()> {
    let current = current_branch(repo)?;
    if current != base_branch {
        return Err(ForjaError::InvalidInput(format!(
            "el repositorio está en la rama '{current}', no en '{base_branch}'; cámbiala antes de fusionar"
        )));
    }
    if !status_clean(repo)? {
        return Err(ForjaError::InvalidInput(format!(
            "el repositorio en '{base_branch}' tiene cambios sin commitear; commitéalos o descártalos antes de fusionar"
        )));
    }
    Ok(())
}

/// `true` si `worktree` figura registrado en `git worktree list` de `repo`.
pub fn worktree_registered(repo: &Path, worktree: &Path) -> Result<bool> {
    let stdout = run_git_cmd(
        repo,
        &["worktree", "list", "--porcelain"],
        "git worktree list",
    )?;
    Ok(stdout.lines().any(|line| {
        line.strip_prefix("worktree ").is_some_and(|registered| {
            let registered = Path::new(registered);
            registered == worktree
                || registered
                    .canonicalize()
                    .is_ok_and(|canonical| canonical == worktree)
        })
    }))
}

/// Elimina el worktree en `worktree`. Con `force` descarta cambios sin commitear.
/// Devuelve `false` si ya no estaba registrado (operación idempotente).
pub fn remove_worktree(repo: &Path, worktree: &Path, force: bool) -> Result<bool> {
    if !worktree_registered(repo, worktree)? {
        // Registro huérfano (carpeta borrada a mano): limpiar metadatos si hace falta.
        run_git_cmd(repo, &["worktree", "prune"], "git worktree prune")?;
        return Ok(false);
    }
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let worktree_arg = worktree.to_string_lossy().into_owned();
    args.push(&worktree_arg);
    run_git_cmd(repo, &args, "git worktree remove")?;
    Ok(true)
}

/// `true` si existe la rama local `branch` en `repo`.
pub fn branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .map_err(|err| ForjaError::Git(format!("no se pudo ejecutar git: {err}")))?;
    Ok(output.status.success())
}

/// Borra la rama local `branch`. Con `force` usa `-D` (descarta commits no
/// fusionados). Devuelve `false` si la rama ya no existía.
pub fn delete_branch(repo: &Path, branch: &str, force: bool) -> Result<bool> {
    if !branch_exists(repo, branch)? {
        return Ok(false);
    }
    let flag = if force { "-D" } else { "-d" };
    run_git_cmd(repo, &["branch", flag, branch], "git branch")?;
    Ok(true)
}

/// Comprueba si el repositorio en `repo` no tiene cambios pendientes.
pub fn status_clean(repo: &Path) -> Result<bool> {
    let stdout = run_git_cmd(repo, &["status", "--porcelain"], "git status --porcelain")?;
    Ok(stdout.trim().is_empty())
}

/// Añade todos los cambios en `repo`, crea un commit con mensaje `msg`
/// y devuelve el hash corto del commit resultante.
pub fn commit_all(repo: &Path, msg: &str) -> Result<String> {
    run_git_cmd(repo, &["add", "-A"], "git add -A")?;
    run_git_cmd(repo, &["commit", "-m", msg], "git commit")?;
    let hash = run_git_cmd(
        repo,
        &["rev-parse", "--short", "HEAD"],
        "git rev-parse --short HEAD",
    )?;
    Ok(hash.trim().to_string())
}

/// Rama a la que apunta `HEAD` en `worktree` (`refs/heads/...` sin el prefijo).
/// `None` si `HEAD` está suelto (detached).
pub fn head_branch(worktree: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .map_err(|err| ForjaError::Git(format!("no se pudo ejecutar git: {err}")))?;
    match output.status.code() {
        Some(0) => {
            let full = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok(Some(
                full.strip_prefix("refs/heads/")
                    .unwrap_or(full.as_str())
                    .to_string(),
            ))
        }
        // Código 1 con -q: HEAD no es una referencia simbólica (suelto).
        Some(1) => Ok(None),
        _ => Err(ForjaError::Git(format!(
            "git symbolic-ref HEAD falló: {}",
            failure_detail(&output)
        ))),
    }
}

/// Hash completo de `rev` en `repo`.
pub fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    let out = run_git_cmd(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
        "git rev-parse",
    )?;
    Ok(out.trim().to_string())
}

/// `true` si `ancestor` ya está contenido en `descendant`.
pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map_err(|err| ForjaError::Git(format!("no se pudo ejecutar git: {err}")))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(ForjaError::Git(format!(
            "git merge-base --is-ancestor falló: {}",
            failure_detail(&output)
        ))),
    }
}

/// Añade `pattern` a `.git/info/exclude` de `repo` si no está ya.
///
/// Sirve para que las carpetas de datos de Forja dentro del repo (`.forja/`) no
/// aparezcan como cambios sin seguimiento sin tocar archivos versionados como
/// `.gitignore`. Funciona también desde un worktree enlazado (usa el
/// `info/exclude` común). Devuelve `true` si escribió algo.
pub fn ensure_excluded(repo: &Path, pattern: &str) -> Result<bool> {
    use std::io::Write as _;

    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.contains('\n') {
        return Err(ForjaError::InvalidInput(
            "patrón de exclusión inválido".to_string(),
        ));
    }
    let common = run_git_cmd(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "git rev-parse --git-common-dir",
    )?;
    let info = PathBuf::from(common.trim()).join("info");
    let exclude = info.join("exclude");
    let current = match std::fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    if current.lines().any(|line| line.trim() == pattern) {
        return Ok(false);
    }
    std::fs::create_dir_all(&info)?;
    let separator = if current.is_empty() || current.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    // Solo se añade al final (nunca se reescribe): así no se pierde lo que otro
    // proceso escriba a la vez ni se trunca el archivo si algo falla a medias.
    let block =
        format!("{separator}# Datos locales de Forja (añadido automáticamente)\n{pattern}\n");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude)?
        .write_all(block.as_bytes())?;
    Ok(true)
}

/// Devuelve el nombre de la rama actual en `repo`.
pub fn current_branch(repo: &Path) -> Result<String> {
    let branch = run_git_cmd(
        repo,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "git rev-parse --abbrev-ref HEAD",
    )?;
    Ok(branch.trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("ejecutar git en test");
        assert!(
            output.status.success(),
            "git {:?} falló:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("ejecutar git en test");
        assert!(
            output.status.success(),
            "git {:?} falló:\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn init_temp_repo() -> TempDir {
        let dir = TempDir::new().expect("crear directorio temporal");
        let repo = dir.path();

        run_git(repo, &["init", "-b", "main"]);
        run_git(repo, &["config", "user.name", "Forja Test"]);
        run_git(repo, &["config", "user.email", "forja-test@example.com"]);

        fs::write(repo.join("README.md"), "# Repo de prueba\n").expect("escribir README");
        run_git(repo, &["add", "README.md"]);
        run_git(repo, &["commit", "-m", "commit inicial"]);

        dir
    }

    #[test]
    fn create_worktree_creates_branch_and_directory() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-tarea-1");

        let created = create_worktree(repo_dir.path(), "main", "forja/tarea-1", &destino)
            .expect("create_worktree debe tener éxito");

        assert_eq!(created, destino);
        assert!(destino.is_dir(), "la carpeta destino debe existir");
        let readme = fs::read_to_string(destino.join("README.md"))
            .expect("README.md debe existir en el worktree");
        assert_eq!(readme, "# Repo de prueba\n");

        let branch_check = Command::new("git")
            .arg("-C")
            .arg(repo_dir.path())
            .args(["rev-parse", "--verify", "refs/heads/forja/tarea-1"])
            .output()
            .expect("verificar rama");
        assert!(
            branch_check.status.success(),
            "la rama forja/tarea-1 debe existir en el repo"
        );
    }

    #[test]
    fn create_worktree_fails_when_base_branch_missing() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-error");

        let err = create_worktree(
            repo_dir.path(),
            "rama-inexistente",
            "forja/tarea-error",
            &destino,
        )
        .expect_err("debe fallar cuando la rama base no existe");

        match err {
            ForjaError::Git(msg) => {
                assert!(
                    !msg.is_empty(),
                    "el mensaje de error de git no debe estar vacío"
                );
                assert!(
                    msg.contains("rama-inexistente"),
                    "el error debe incluir contexto útil: {msg}"
                );
            }
            other => panic!("se esperaba ForjaError::Git, se obtuvo: {other:?}"),
        }
        assert!(
            !destino.exists(),
            "la carpeta destino no debe haberse creado tras un error"
        );
    }

    #[test]
    fn status_clean_detects_clean_and_dirty_repo() {
        let repo_dir = init_temp_repo();

        assert!(
            status_clean(repo_dir.path()).expect("status_clean en repo limpio"),
            "el repo inicial debe estar limpio"
        );

        fs::write(repo_dir.path().join("nuevo.txt"), "contenido\n").expect("crear archivo");
        assert!(
            !status_clean(repo_dir.path()).expect("status_clean tras crear archivo"),
            "el repo debe marcarse sucio al crear un archivo"
        );

        run_git(repo_dir.path(), &["add", "nuevo.txt"]);
        run_git(repo_dir.path(), &["commit", "-m", "añadir nuevo.txt"]);
        assert!(
            status_clean(repo_dir.path()).expect("status_clean tras commit"),
            "el repo debe volver a estar limpio tras commitear"
        );

        fs::write(repo_dir.path().join("README.md"), "# Modificado\n").expect("modificar README");
        assert!(
            !status_clean(repo_dir.path()).expect("status_clean tras modificar archivo"),
            "el repo debe marcarse sucio al modificar un archivo existente"
        );
    }

    #[test]
    fn commit_all_commits_changes_and_fails_when_nothing_to_commit() {
        let repo_dir = init_temp_repo();

        fs::write(repo_dir.path().join("cambios.txt"), "linea 1\n").expect("crear cambios.txt");
        assert!(!status_clean(repo_dir.path()).expect("status_clean antes de commit_all"));

        let hash = commit_all(repo_dir.path(), "añadir cambios")
            .expect("commit_all debe tener éxito con cambios pendientes");
        assert!(!hash.is_empty(), "el hash devuelto no debe estar vacío");
        assert!(
            status_clean(repo_dir.path()).expect("status_clean después de commit_all"),
            "el repo debe quedar limpio tras commit_all"
        );

        let err = commit_all(repo_dir.path(), "commit vacío")
            .expect_err("commit_all debe fallar si no hay cambios");
        match err {
            ForjaError::Git(msg) => {
                assert!(!msg.is_empty(), "el mensaje de error no debe estar vacío");
            }
            other => panic!("se esperaba ForjaError::Git, se obtuvo: {other:?}"),
        }
    }

    #[test]
    fn diff_worktree_shows_committed_and_uncommitted_changes() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-diff");
        create_worktree(repo_dir.path(), "main", "forja/diff", &destino).expect("crear worktree");

        fs::write(destino.join("nuevo.txt"), "linea nueva\n").expect("escribir archivo");
        commit_all(&destino, "añadir nuevo.txt").expect("commit en worktree");
        fs::write(
            destino.join("README.md"),
            "# Repo modificado sin commitear\n",
        )
        .expect("modificar README");

        let diff = diff_worktree(&destino, "main", 64 * 1024).expect("diff del worktree");
        assert!(
            diff.text.contains("nuevo.txt"),
            "el diff debe incluir el archivo commiteado"
        );
        assert!(
            diff.text.contains("Repo modificado sin commitear"),
            "el diff debe incluir cambios sin commitear: {}",
            diff.text
        );
        assert!(
            diff.stat.contains("nuevo.txt"),
            "el resumen debe listar los archivos: {}",
            diff.stat
        );
        assert!(!diff.truncated, "un diff pequeño no debe truncarse");

        // Los archivos sin seguimiento también se ven y el índice queda intacto.
        fs::write(destino.join("sin-seguimiento.txt"), "nuevo sin add\n")
            .expect("crear archivo sin seguimiento");
        let status_before = run_git_stdout(&destino, &["status", "--porcelain"]);
        let con_untracked = diff_worktree(&destino, "main", 64 * 1024).expect("diff con untracked");
        assert!(
            con_untracked.text.contains("sin-seguimiento.txt")
                && con_untracked.text.contains("+nuevo sin add"),
            "el diff debe incluir archivos sin seguimiento: {}",
            con_untracked.text
        );
        assert!(
            con_untracked.stat.contains("sin-seguimiento.txt"),
            "el resumen debe listar el archivo sin seguimiento: {}",
            con_untracked.stat
        );
        assert_eq!(
            status_before,
            run_git_stdout(&destino, &["status", "--porcelain"]),
            "revisar el diff no debe dejar cambios en el índice"
        );

        // La base avanzó (otro worktree ya fusionado): el diff de esta tarea se
        // calcula desde su merge-base y no debe incluir los commits ajenos.
        fs::write(repo_dir.path().join("otra-tarea.txt"), "de la base\n")
            .expect("avanzar la rama base");
        commit_all(repo_dir.path(), "avance de la base").expect("commit en la base");
        let tras_avance = diff_worktree(&destino, "main", 64 * 1024).expect("diff tras avanzar");
        assert!(
            tras_avance.text.contains("nuevo.txt"),
            "el diff debe conservar los cambios de la tarea: {}",
            tras_avance.text
        );
        assert!(
            !tras_avance.text.contains("otra-tarea.txt")
                && !tras_avance.stat.contains("otra-tarea.txt"),
            "el diff no debe incluir commits fusionados después en la base: {}",
            tras_avance.text
        );

        let short = diff_worktree(&destino, "main", 20).expect("diff truncado");
        assert!(short.truncated, "debe marcarse como truncado");
        assert!(
            short.text.ends_with("…[diff truncado]\n"),
            "debe avisar del recorte: {}",
            short.text
        );
        assert!(
            std::str::from_utf8(short.text.as_bytes()).is_ok(),
            "el recorte debe respetar UTF-8"
        );
    }

    #[test]
    fn merge_branch_moves_changes_to_base_and_reports_up_to_date() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-merge");
        create_worktree(repo_dir.path(), "main", "forja/merge", &destino).expect("crear worktree");
        fs::write(destino.join("fusion.txt"), "fusionado\n").expect("escribir archivo");
        commit_all(&destino, "trabajo de la tarea").expect("commit en worktree");

        let outcome = merge_branch(repo_dir.path(), "main", "forja/merge", "fusionar tarea")
            .expect("merge debe tener éxito");
        assert!(!outcome.already_up_to_date);
        assert!(!outcome.commit.is_empty());
        assert_eq!(
            fs::read_to_string(repo_dir.path().join("fusion.txt")).expect("leer archivo fusionado"),
            "fusionado\n"
        );
        assert!(
            status_clean(repo_dir.path()).expect("status tras merge"),
            "el repo base debe quedar limpio"
        );

        let again = merge_branch(repo_dir.path(), "main", "forja/merge", "fusionar otra vez")
            .expect("un segundo merge no debe fallar");
        assert!(
            again.already_up_to_date,
            "debe detectar 'Already up to date'"
        );
    }

    #[test]
    fn merge_branch_requires_base_branch_and_clean_tree() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-pre");
        create_worktree(repo_dir.path(), "main", "forja/pre", &destino).expect("crear worktree");
        fs::write(destino.join("x.txt"), "x\n").expect("escribir archivo");
        commit_all(&destino, "x").expect("commit en worktree");

        run_git(repo_dir.path(), &["checkout", "-q", "-b", "otra"]);
        let err = merge_branch(repo_dir.path(), "main", "forja/pre", "merge")
            .expect_err("debe fallar fuera de la rama base");
        assert!(matches!(err, ForjaError::InvalidInput(_)), "{err:?}");
        run_git(repo_dir.path(), &["checkout", "-q", "main"]);

        fs::write(repo_dir.path().join("sucio.txt"), "sucio\n").expect("ensuciar repo");
        let err = merge_branch(repo_dir.path(), "main", "forja/pre", "merge")
            .expect_err("debe fallar con cambios sin commitear");
        assert!(matches!(err, ForjaError::InvalidInput(_)), "{err:?}");
    }

    #[test]
    fn merge_branch_aborts_conflict_and_leaves_repo_clean() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-conflicto");
        create_worktree(repo_dir.path(), "main", "forja/conflicto", &destino)
            .expect("crear worktree");

        fs::write(destino.join("README.md"), "# Worktree\n").expect("modificar README en wt");
        commit_all(&destino, "cambio en worktree").expect("commit en worktree");
        fs::write(repo_dir.path().join("README.md"), "# Base\n").expect("modificar README en base");
        commit_all(repo_dir.path(), "cambio en base").expect("commit en base");

        let err = merge_branch(repo_dir.path(), "main", "forja/conflicto", "fusionar")
            .expect_err("debe detectar conflicto");
        assert!(matches!(err, ForjaError::Conflict(_)), "{err:?}");
        assert!(
            status_clean(repo_dir.path()).expect("status tras conflicto"),
            "el merge abortado debe dejar el repo limpio"
        );
    }

    #[test]
    fn remove_worktree_and_delete_branch_discard_task() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-descartar");
        create_worktree(repo_dir.path(), "main", "forja/descartar", &destino)
            .expect("crear worktree");
        fs::write(destino.join("desecho.txt"), "no fusionar\n").expect("escribir archivo");
        commit_all(&destino, "trabajo descartable").expect("commit en worktree");

        assert!(worktree_registered(repo_dir.path(), &destino).expect("listar worktrees"));
        assert!(remove_worktree(repo_dir.path(), &destino, true).expect("quitar worktree"));
        assert!(
            !destino.exists(),
            "la carpeta del worktree debe desaparecer"
        );
        assert!(
            !worktree_registered(repo_dir.path(), &destino).expect("listar tras quitar"),
            "el worktree no debe seguir registrado"
        );

        assert!(branch_exists(repo_dir.path(), "forja/descartar").expect("comprobar rama"));
        assert!(delete_branch(repo_dir.path(), "forja/descartar", true).expect("borrar rama"));
        assert!(
            !branch_exists(repo_dir.path(), "forja/descartar").expect("comprobar tras borrar"),
            "la rama descartada no debe existir"
        );
        assert!(
            !remove_worktree(repo_dir.path(), &destino, true).expect("segunda limpieza"),
            "quitar un worktree inexistente es idempotente"
        );
        assert!(
            !delete_branch(repo_dir.path(), "forja/descartar", true).expect("segundo borrado"),
            "borrar una rama inexistente es idempotente"
        );
    }

    #[test]
    fn remove_worktree_requires_force_for_uncommitted_changes() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-sucio");
        create_worktree(repo_dir.path(), "main", "forja/sucio", &destino).expect("crear worktree");
        fs::write(destino.join("pendiente.txt"), "sin commitear\n").expect("ensuciar worktree");

        let err = remove_worktree(repo_dir.path(), &destino, false)
            .expect_err("sin force no debe borrar un worktree sucio");
        assert!(matches!(err, ForjaError::Git(_)), "{err:?}");
        assert!(destino.is_dir(), "el worktree debe seguir ahí");

        assert!(
            remove_worktree(repo_dir.path(), &destino, true).expect("forzar borrado"),
            "con force sí debe eliminarse"
        );
        assert!(!destino.exists(), "la carpeta debe desaparecer");
    }

    #[test]
    fn current_branch_returns_main_and_worktree_branch() {
        let repo_dir = init_temp_repo();
        let wt_parent = TempDir::new().expect("crear padre de worktree");
        let destino = wt_parent.path().join("wt-rama");

        let main_branch = current_branch(repo_dir.path()).expect("current_branch en repo inicial");
        assert_eq!(main_branch, "main");

        let wt_path = create_worktree(repo_dir.path(), "main", "forja/nueva-rama", &destino)
            .expect("create_worktree debe tener éxito");
        let wt_branch = current_branch(&wt_path).expect("current_branch en worktree");
        assert_eq!(wt_branch, "forja/nueva-rama");
    }

    #[test]
    fn head_branch_detects_detached_and_other_branches() {
        let repo_dir = init_temp_repo();
        let repo = repo_dir.path();
        assert_eq!(head_branch(repo).unwrap().as_deref(), Some("main"));
        run_git(repo, &["checkout", "-q", "-b", "otra"]);
        assert_eq!(head_branch(repo).unwrap().as_deref(), Some("otra"));
        run_git(repo, &["checkout", "-q", "--detach"]);
        assert_eq!(head_branch(repo).unwrap(), None);
        let head = rev_parse(repo, "HEAD").unwrap();
        assert!(is_ancestor(repo, &head, "main").unwrap());
        fs::write(repo.join("x.txt"), "x").unwrap();
        run_git(repo, &["add", "x.txt"]);
        run_git(repo, &["commit", "-qm", "suelto"]);
        assert!(!is_ancestor(repo, "HEAD", "main").unwrap());
    }

    #[test]
    fn ensure_excluded_hides_forja_dir_without_touching_gitignore() {
        let repo_dir = init_temp_repo();
        let repo = repo_dir.path();
        fs::create_dir_all(repo.join(".forja/store")).unwrap();
        fs::write(repo.join(".forja/store/runs.jsonl"), "{}\n").unwrap();
        assert!(!status_clean(repo).unwrap());
        assert!(ensure_excluded(repo, "/.forja/").unwrap());
        assert!(!ensure_excluded(repo, "/.forja/").unwrap(), "idempotente");
        assert!(status_clean(repo).unwrap());
        assert!(!repo.join(".gitignore").exists());
        // Desde un worktree enlazado escribe en el exclude común.
        let wt_parent = TempDir::new().unwrap();
        let wt = wt_parent.path().join("wt");
        create_worktree(repo, "main", "forja/x", &wt).unwrap();
        assert!(!ensure_excluded(&wt, "/.forja/").unwrap());
    }

    #[test]
    fn ensure_excluded_keeps_every_pattern_under_concurrency() {
        let repo_dir = init_temp_repo();
        let repo = repo_dir.path();
        let exclude = repo.join(".git/info/exclude");
        fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        fs::write(&exclude, "propio-del-usuario").unwrap();
        std::thread::scope(|scope| {
            for i in 0..12 {
                scope.spawn(move || ensure_excluded(repo, &format!("/dir-{i}/")).unwrap());
            }
        });
        let text = fs::read_to_string(&exclude).unwrap();
        let has = |line: &str| text.lines().any(|l| l == line);
        assert!(has("propio-del-usuario"), "{text}");
        for i in 0..12 {
            assert!(has(&format!("/dir-{i}/")), "falta /dir-{i}/:\n{text}");
        }
    }
}
