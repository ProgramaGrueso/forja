//! Revisión y cierre del ciclo de una ejecución: diff del worktree, fusión de
//! la rama de tarea y descarte/limpieza.
//!
//! Todas las rutas exigen que la ejecución exista, que no siga activa y que la
//! rama registrada sea exactamente `forja/<id>`; el worktree, si existe, debe
//! colgar de la carpeta de worktrees del planificador. El repositorio base debe
//! estar en su rama base y limpio para fusionar.

use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, State};
use axum::Json;
use forja_core::{ForjaError, Result, RunRecord, RunStatus};
use serde::Serialize;

use crate::{ApiError, AppState};

/// Tope del diff devuelto al panel (bytes UTF-8).
const MAX_DIFF_BYTES: usize = 512 * 1024;

#[derive(Debug, Serialize)]
pub struct DiffResponse {
    pub branch: String,
    pub base: String,
    pub worktree: String,
    pub stat: String,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct ReviewActionResponse {
    pub branch: String,
    pub base: String,
    pub merged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Commit creado con los cambios sin commitear del worktree antes de fusionar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_commit: Option<String>,
    pub already_up_to_date: bool,
    pub worktree_removed: bool,
    pub branch_deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

struct ReviewTarget {
    run: RunRecord,
    repo: PathBuf,
    base: String,
    branch: String,
    /// `None` si el worktree ya no existe (solo se permite descartar).
    worktree: Option<PathBuf>,
}

impl ReviewTarget {
    fn require_worktree(&self) -> Result<(&Path, &str)> {
        let worktree = self.worktree.as_deref().ok_or_else(|| {
            ForjaError::Conflict(
                "el worktree de esta ejecución ya no existe (¿ciclo ya cerrado?)".to_string(),
            )
        })?;
        Ok((worktree, self.branch.as_str()))
    }
}

/// Encadena avisos sin perder los anteriores.
fn join_warning(current: Option<String>, next: &str) -> String {
    match current {
        Some(prev) => format!("{prev}; {next}"),
        None => next.to_string(),
    }
}

/// La rama registrada debe ser exactamente `forja/<id>` y distinta de la base.
fn validate_branch(id: &str, registered: Option<&str>, base: &str) -> Result<String> {
    let expected = format!("forja/{id}");
    let branch = registered.filter(|b| *b == expected).ok_or_else(|| {
        ForjaError::InvalidInput(format!(
            "la rama registrada no es '{expected}'; se rechaza operar sobre otra rama"
        ))
    })?;
    if branch == base {
        return Err(ForjaError::InvalidInput(
            "la rama de la tarea coincide con la rama base".to_string(),
        ));
    }
    Ok(branch.to_string())
}

/// El worktree de una ejecución vive siempre en `<worktrees_dir>/<id>`.
///
/// Un registro manipulado no puede apuntar al worktree de otra ejecución: se
/// compara la ruta registrada con la esperada (también tras resolver enlaces).
fn task_worktree(id: &str, recorded: Option<&str>, worktrees_dir: &Path) -> Result<PathBuf> {
    let expected = worktrees_dir.join(id);
    if let Some(recorded) = recorded {
        let recorded = PathBuf::from(recorded);
        let same = recorded == expected
            || matches!(
                (recorded.canonicalize(), expected.canonicalize()),
                (Ok(a), Ok(b)) if a == b
            );
        if !same {
            return Err(ForjaError::InvalidInput(
                "el worktree registrado no corresponde a esta ejecución".to_string(),
            ));
        }
    }
    Ok(expected)
}

/// Valida que `candidate` está dentro de `base` tras resolver enlaces.
/// Devuelve `Ok(None)` si la ruta no existe.
fn worktree_inside(base: &Path, candidate: &Path) -> Result<Option<PathBuf>> {
    let Ok(canonical_candidate) = candidate.canonicalize() else {
        return Ok(None);
    };
    let canonical_base = base.canonicalize().map_err(|_| {
        ForjaError::NotConfigured(
            "la carpeta de worktrees no existe; el servidor no tiene repo configurado".to_string(),
        )
    })?;
    if !canonical_candidate.starts_with(&canonical_base) {
        return Err(ForjaError::InvalidInput(
            "el worktree registrado está fuera de la carpeta de worktrees".to_string(),
        ));
    }
    Ok(Some(canonical_candidate))
}

async fn resolve(state: &AppState, id: &str) -> Result<ReviewTarget> {
    if !forja_sched::is_valid_run_id(id) {
        return Err(ForjaError::NotFound(id.to_string()));
    }
    let run = state
        .scheduler
        .get_run(id)
        .await
        .ok_or_else(|| ForjaError::NotFound(id.to_string()))?;
    if matches!(run.status, RunStatus::Pending | RunStatus::Running) {
        return Err(ForjaError::Conflict(
            "la ejecución sigue activa; espera a que termine o cancélala".to_string(),
        ));
    }

    let repo = state.scheduler.repo_path().ok_or_else(|| {
        ForjaError::NotConfigured(
            "el servidor no se inició con un repo git (--repo); no hay worktrees que revisar"
                .to_string(),
        )
    })?;
    let base = state.scheduler.base_branch();
    let branch = validate_branch(id, run.branch.as_deref(), &base)?;

    let worktrees_dir = state.scheduler.worktrees_dir();
    let raw_worktree = task_worktree(id, run.worktree.as_deref(), &worktrees_dir)?;
    let worktree = worktree_inside(&worktrees_dir, &raw_worktree)?;

    Ok(ReviewTarget {
        run,
        repo,
        base,
        branch,
        worktree,
    })
}

/// `GET /runs/{id}/diff`: diff del worktree contra la rama base.
pub(crate) async fn get_diff(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> std::result::Result<Json<DiffResponse>, ApiError> {
    let _guard = state.review_lock.lock().await;
    let target = resolve(&state, &id).await?;
    let (worktree, branch) = target.require_worktree()?;
    let worktree = worktree.to_path_buf();
    let base = target.base.clone();
    let branch = branch.to_string();

    let (worktree_arg, base_arg) = (worktree.clone(), base.clone());
    let diff = tokio::task::spawn_blocking(move || {
        forja_git::diff_worktree(&worktree_arg, &base_arg, MAX_DIFF_BYTES)
    })
    .await
    .map_err(|err| ForjaError::Git(format!("fallo al calcular el diff: {err}")))??;

    Ok(Json(DiffResponse {
        branch,
        base,
        worktree: worktree.to_string_lossy().into_owned(),
        stat: diff.stat,
        diff: diff.text,
        truncated: diff.truncated,
    }))
}

/// `POST /runs/{id}/merge`: fusiona la rama de la tarea en la rama base y
/// retira el worktree. La rama se borra solo si git la considera fusionada.
pub(crate) async fn merge(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> std::result::Result<Json<ReviewActionResponse>, ApiError> {
    let _guard = state.review_lock.lock().await;
    let target = resolve(&state, &id).await?;
    if target.run.status != RunStatus::Done {
        return Err(ApiError(ForjaError::Conflict(format!(
            "solo se fusionan ejecuciones completadas con éxito ('done'), no '{}'; usa descartar para el resto",
            target.run.status
        ))));
    }
    let (worktree, branch) = target.require_worktree()?;
    let (repo, base, branch, worktree) = (
        target.repo.clone(),
        target.base.clone(),
        branch.to_string(),
        worktree.to_path_buf(),
    );

    // Si alguien tocó el worktree después de la tarea, sus cambios también
    // entran en el diff: se commitean en la rama antes de fusionarla.
    let (worktree_arg, commit_message) = (
        worktree.clone(),
        format!("forja: cambios pendientes de {branch}"),
    );
    let worktree_commit = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
        if forja_git::status_clean(&worktree_arg)? {
            return Ok(None);
        }
        forja_git::commit_all(&worktree_arg, &commit_message).map(Some)
    })
    .await
    .map_err(|err| ForjaError::Git(format!("fallo al commitear el worktree: {err}")))??;

    let (repo_arg, base_arg, branch_arg, message) = (
        repo.clone(),
        base.clone(),
        branch.clone(),
        format!("forja: fusionar {branch}"),
    );
    let outcome = tokio::task::spawn_blocking(move || {
        forja_git::merge_branch(&repo_arg, &base_arg, &branch_arg, &message)
    })
    .await
    .map_err(|err| ForjaError::Git(format!("fallo al fusionar: {err}")))??;

    // La fusión ya ocurrió: un fallo de limpieza se informa, no se deshace.
    let (repo_arg, worktree_arg) = (repo.clone(), worktree.clone());
    let removed = tokio::task::spawn_blocking(move || {
        forja_git::remove_worktree(&repo_arg, &worktree_arg, false)
    })
    .await
    .map_err(|err| ForjaError::Git(format!("fallo al retirar el worktree: {err}")))?;
    let (worktree_removed, mut warning) = match removed {
        Ok(true) => (true, None),
        Ok(false) => (
            false,
            Some(
                "el directorio del worktree no estaba registrado en git; bórralo a mano si sigue ahí"
                    .to_string(),
            ),
        ),
        Err(err) => (false, Some(err.to_string())),
    };

    let mut branch_deleted = false;
    if worktree_removed {
        let (repo_arg, branch_arg) = (repo.clone(), branch.clone());
        let deleted = tokio::task::spawn_blocking(move || {
            forja_git::delete_branch(&repo_arg, &branch_arg, false)
        })
        .await
        .map_err(|err| ForjaError::Git(format!("fallo al borrar la rama: {err}")))?;
        match deleted {
            Ok(true) => branch_deleted = true,
            Ok(false) => {}
            Err(err) => warning = Some(join_warning(warning, &format!("al borrar la rama: {err}"))),
        }
    }

    Ok(Json(ReviewActionResponse {
        branch,
        base,
        merged: true,
        commit: Some(outcome.commit),
        worktree_commit,
        already_up_to_date: outcome.already_up_to_date,
        worktree_removed,
        branch_deleted,
        warning,
    }))
}

/// `POST /runs/{id}/discard`: descarta el trabajo de la tarea (cambios sin
/// commitear y commits de la rama) y limpia el worktree.
pub(crate) async fn discard(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> std::result::Result<Json<ReviewActionResponse>, ApiError> {
    let _guard = state.review_lock.lock().await;
    let target = resolve(&state, &id).await?;
    let (repo, base, branch, worktree) = (
        target.repo.clone(),
        target.base.clone(),
        target.branch.clone(),
        target.worktree.clone(),
    );

    let (repo_arg, worktree_arg) = (repo.clone(), worktree.clone());
    let removed = tokio::task::spawn_blocking(move || match worktree_arg {
        Some(path) => forja_git::remove_worktree(&repo_arg, &path, true),
        None => Ok(false),
    })
    .await
    .map_err(|err| ForjaError::Git(format!("fallo al retirar el worktree: {err}")))?;
    let worktree_expected = worktree.is_some();
    let mut failed = false;
    let (worktree_removed, mut warning) = match removed {
        Ok(true) => (true, None),
        Ok(false) if worktree_expected => (
            false,
            Some(
                "el directorio del worktree no estaba registrado en git; bórralo a mano si sigue ahí"
                    .to_string(),
            ),
        ),
        Ok(false) => (false, None),
        Err(err) => {
            failed = true;
            (false, Some(err.to_string()))
        }
    };

    let (repo_arg, branch_arg) = (repo.clone(), branch.clone());
    let deleted =
        tokio::task::spawn_blocking(move || forja_git::delete_branch(&repo_arg, &branch_arg, true))
            .await
            .map_err(|err| ForjaError::Git(format!("fallo al borrar la rama: {err}")))?;
    let mut branch_deleted = false;
    match deleted {
        Ok(true) => branch_deleted = true,
        Ok(false) => {}
        Err(err) => {
            failed = true;
            warning = Some(join_warning(warning, &format!("al borrar la rama: {err}")));
        }
    }

    if failed && !worktree_removed && !branch_deleted {
        return Err(ApiError(ForjaError::Git(
            warning.unwrap_or_else(|| "no se pudo limpiar el worktree".to_string()),
        )));
    }

    Ok(Json(ReviewActionResponse {
        branch,
        base,
        merged: false,
        commit: None,
        worktree_commit: None,
        already_up_to_date: false,
        worktree_removed,
        branch_deleted,
        warning,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn validate_branch_only_accepts_the_task_branch() {
        let id = "abc-123";
        assert_eq!(
            validate_branch(id, Some("forja/abc-123"), "main").expect("rama válida"),
            "forja/abc-123"
        );
        for registered in [
            None,
            Some("main"),
            Some("forja/otra"),
            Some("forja/abc-1234"),
            Some("forja/abc-123 "),
        ] {
            let err =
                validate_branch(id, registered, "main").expect_err("solo debe aceptar forja/<id>");
            assert!(matches!(err, ForjaError::InvalidInput(_)), "{err:?}");
        }
        let err = validate_branch("main", Some("forja/main"), "forja/main")
            .expect_err("la rama base nunca se revisa");
        assert!(matches!(err, ForjaError::InvalidInput(_)), "{err:?}");
    }

    #[test]
    fn task_worktree_is_always_the_run_directory() {
        let base = TempDir::new().expect("base tempdir");
        let expected = base.path().join("run-1");

        assert_eq!(
            task_worktree("run-1", None, base.path()).expect("sin ruta registrada"),
            expected
        );
        assert_eq!(
            task_worktree(
                "run-1",
                Some(expected.to_str().expect("ruta UTF-8")),
                base.path()
            )
            .expect("ruta registrada correcta"),
            expected
        );

        for recorded in [
            base.path().join("run-2"),
            base.path().join("run-1").join("..").join("run-2"),
            TempDir::new().expect("fuera").path().join("run-1"),
        ] {
            let err = task_worktree(
                "run-1",
                Some(recorded.to_str().expect("ruta UTF-8")),
                base.path(),
            )
            .expect_err("un worktree de otra ejecución debe rechazarse");
            assert!(matches!(err, ForjaError::InvalidInput(_)), "{err:?}");
        }
    }

    #[test]
    fn worktree_inside_confines_paths_to_the_worktrees_dir() {
        let base = TempDir::new().expect("base tempdir");
        let inside = base.path().join("wt-tarea");
        fs::create_dir(&inside).expect("crear worktree");

        let resolved = worktree_inside(base.path(), &inside)
            .expect("ruta interna")
            .expect("debe existir");
        assert_eq!(
            resolved,
            inside.canonicalize().expect("canonicalizar worktree")
        );

        let fuera = TempDir::new().expect("tempdir externo");
        let err = worktree_inside(base.path(), fuera.path())
            .expect_err("una ruta externa debe rechazarse");
        assert!(matches!(err, ForjaError::InvalidInput(_)), "{err:?}");

        #[cfg(unix)]
        {
            let enlace = base.path().join("enlace-escape");
            std::os::unix::fs::symlink(fuera.path(), &enlace).expect("crear symlink");
            let err = worktree_inside(base.path(), &enlace)
                .expect_err("un symlink que sale de la carpeta debe rechazarse");
            assert!(matches!(err, ForjaError::InvalidInput(_)), "{err:?}");
        }

        let missing = base.path().join("no-existe");
        assert!(
            worktree_inside(base.path(), &missing)
                .expect("ruta ausente")
                .is_none(),
            "un worktree ya borrado se trata como ausente"
        );
    }
}
