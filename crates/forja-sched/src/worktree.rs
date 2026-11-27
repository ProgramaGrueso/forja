use std::path::Path;

use forja_core::{ForjaError, Result};
use tokio::sync::Mutex;

pub(crate) async fn prepare_worktree(
    repo_path: Option<&Path>,
    worktrees_dir: &Path,
    store_dir: &Path,
    base_branch: &str,
    git_lock: &Mutex<()>,
    wt_path: &Path,
    branch: Option<&str>,
) -> Result<()> {
    if let (Some(repo), Some(branch_name)) = (repo_path, branch) {
        tokio::fs::create_dir_all(worktrees_dir).await?;
        // Sin esto, en un repo sin `.forja/` en su .gitignore la base queda con
        // `?? .forja/` y toda fusión falla por "cambios sin commitear".
        exclude_internal_dirs(repo, &[worktrees_dir, store_dir])?;

        let repo_owned = repo.to_path_buf();
        let base_branch = base_branch.to_string();
        let branch_owned = branch_name.to_string();
        let wt_owned = wt_path.to_path_buf();

        let _git_guard = git_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            forja_git::create_worktree(&repo_owned, &base_branch, &branch_owned, &wt_owned)
        })
        .await
        .map_err(|err| ForjaError::Git(format!("fallo al crear worktree: {err}")))??;
    } else {
        tokio::fs::create_dir_all(wt_path).await?;
    }

    Ok(())
}

pub(crate) async fn commit_worktree_changes(wt_path: &Path, task_title: &str) -> Result<()> {
    let wt_owned = wt_path.to_path_buf();
    let commit_msg = format!("forja: {task_title}");
    tokio::task::spawn_blocking(move || -> Result<()> {
        if !forja_git::status_clean(&wt_owned)? {
            let _ = forja_git::commit_all(&wt_owned, &commit_msg)?;
        }
        Ok(())
    })
    .await
    .map_err(|err| ForjaError::Git(format!("fallo al commitear cambios: {err}")))??;

    Ok(())
}

/// Añade a `.git/info/exclude` del repo cada carpeta de `dirs` que viva dentro
/// de él (patrón anclado `/<ruta>/`). No toca archivos versionados.
pub(crate) fn exclude_internal_dirs(repo: &Path, dirs: &[&Path]) -> Result<()> {
    let repo_abs = repo.canonicalize()?;
    for dir in dirs {
        let abs = match dir.canonicalize() {
            Ok(path) => path,
            // Aún no existe: resolver contra el cwd sin seguir enlaces.
            Err(_) => std::path::absolute(dir)?,
        };
        let Ok(rel) = abs.strip_prefix(&repo_abs) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.is_empty() || rel.starts_with(".git/") || rel == ".git" {
            continue;
        }
        forja_git::ensure_excluded(repo, &format!("/{rel}/"))?;
    }
    Ok(())
}
