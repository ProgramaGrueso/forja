use std::path::Path;

use forja_core::{ForjaError, Result};
use tokio::sync::Mutex;

pub(crate) async fn prepare_worktree(
    repo_path: Option<&Path>,
    worktrees_dir: &Path,
    base_branch: &str,
    git_lock: &Mutex<()>,
    wt_path: &Path,
    branch: Option<&str>,
) -> Result<()> {
    if let (Some(repo), Some(branch_name)) = (repo_path, branch) {
        tokio::fs::create_dir_all(worktrees_dir).await?;

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
