use axum::extract::{Path, State};
use axum::Json;
use forja_core::{ForjaError, RunRecord};

use crate::handlers::ApiError;
use crate::state::AppState;

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
