use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use forja_core::{ForjaError, RunRecord, RunStatus};

use crate::handlers::ApiError;
use crate::state::AppState;
use crate::CreateTaskRequest;

pub(crate) async fn list_runs(State(state): State<AppState>) -> Json<Vec<RunRecord>> {
    Json(state.scheduler.list_runs().await)
}

pub(crate) async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<RunRecord>, ApiError> {
    let record = state
        .scheduler
        .get_run(&id)
        .await
        .ok_or(ApiError(ForjaError::NotFound(id)))?;
    Ok(Json(record))
}

pub(crate) async fn cancel_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<RunRecord>, ApiError> {
    let record = state.scheduler.cancel_run(&id).await?;
    Ok(Json(record))
}

/// `POST /runs/{id}/retry`: relanza una ejecución terminada (`done`, `failed` o
/// `cancelled`) como una tarea NUEVA con el mismo título, prompt, agente,
/// modelo, esfuerzo y privacidad, pasando por las mismas validaciones y política
/// que `POST /tasks` (en el repo y rama base activos). Responde 201 con el run
/// nuevo; 404 si no existe; 409 si sigue activa o si su prompt se guardó con
/// secretos redactados (relanzarla mandaría `[REDACTADO]` al agente: hay que
/// crearla de nuevo con `POST /tasks` y el prompt completo).
pub(crate) async fn retry_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<(StatusCode, Json<RunRecord>), ApiError> {
    if !forja_sched::is_valid_run_id(&id) {
        return Err(ApiError(ForjaError::NotFound(id)));
    }
    let run = state
        .scheduler
        .get_run(&id)
        .await
        .ok_or_else(|| ApiError(ForjaError::NotFound(id.clone())))?;
    if matches!(run.status, RunStatus::Pending | RunStatus::Running) {
        return Err(ApiError(ForjaError::Conflict(
            "la ejecución sigue activa; espera a que termine o cancélala antes de relanzarla"
                .to_string(),
        )));
    }
    if run.prompt_redacted {
        return Err(ApiError(ForjaError::Conflict(
            "el prompt de esta ejecución se guardó con secretos redactados y relanzarla \
             enviaría el texto incompleto; créala de nuevo con el prompt completo"
                .to_string(),
        )));
    }
    let req = CreateTaskRequest {
        titulo: run.task.titulo,
        prompt: run.task.prompt,
        privacidad: run.task.privacidad,
        agent: run.agent,
        modelo: run.model,
        esfuerzo: run.effort,
    };
    let record = state.submit_task(&req).await?;
    Ok((StatusCode::CREATED, Json(record)))
}
