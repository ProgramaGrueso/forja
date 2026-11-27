use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use forja_core::{AgentKind, Privacy, RunRecord};
use serde::{Deserialize, Serialize};

use crate::handlers::ApiError;
use crate::state::AppState;

fn default_privacy() -> Privacy {
    Privacy::Open
}

fn default_agent() -> AgentKind {
    AgentKind::Mock
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CreateTaskRequest {
    pub titulo: String,
    pub prompt: String,
    #[serde(default = "default_privacy")]
    pub privacidad: Privacy,
    #[serde(default = "default_agent")]
    pub agent: AgentKind,
    /// Override de modelo solo para esta tarea (prioridad sobre los ajustes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modelo: Option<String>,
    /// Override de esfuerzo de razonamiento solo para esta tarea.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub esfuerzo: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct TaskQuery {
    /// Conversación del orquestador que delega (Hermes lo pone en la URL del curl).
    #[serde(default)]
    conversation_id: Option<String>,
}

pub(crate) async fn create_task(
    State(state): State<AppState>,
    Query(query): Query<TaskQuery>,
    Json(req): Json<CreateTaskRequest>,
) -> std::result::Result<(StatusCode, Json<RunRecord>), ApiError> {
    let record = state.submit_task(&req).await?;
    if let Some(conversation) = query
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        // Hermes delega con curl y el servidor no ve sus bloques: esta pista en
        // la URL permite avisarle igual que a los orquestadores one-shot.
        state.track_delegation(conversation, &record.id);
    }
    Ok((StatusCode::CREATED, Json(record)))
}
