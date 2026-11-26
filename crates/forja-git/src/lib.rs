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
}
