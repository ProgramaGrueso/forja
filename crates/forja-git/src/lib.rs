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

fn run_git_cmd(repo: &Path, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
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
        return Err(ForjaError::Git(format!("{context} falló: {detail}")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
}
