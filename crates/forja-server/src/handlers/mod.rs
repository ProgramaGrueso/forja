pub(crate) mod mascot;
pub(crate) mod panel;
pub(crate) mod runs;
pub(crate) mod stream;
pub(crate) mod tasks;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use forja_core::ForjaError;
use serde::Serialize;

pub use mascot::mascot_bytes;
pub(crate) use mascot::mascot_handler;
pub(crate) use panel::panel_handler;
pub(crate) use runs::{cancel_run, get_run, list_runs, retry_run};
pub(crate) use stream::{events_handler, run_output, run_stream};
pub(crate) use tasks::create_task;
pub use tasks::CreateTaskRequest;

#[derive(Debug, Serialize)]
pub(crate) struct ErrorBody {
    pub(crate) error: String,
}

pub(crate) struct ApiError(pub(crate) ForjaError);

impl From<ForjaError> for ApiError {
    fn from(err: ForjaError) -> Self {
        Self(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            ForjaError::PolicyDenied { .. } => StatusCode::FORBIDDEN,
            ForjaError::InvalidInput(_) | ForjaError::NotConfigured(_) => StatusCode::BAD_REQUEST,
            ForjaError::Conflict(_) => StatusCode::CONFLICT,
            ForjaError::NotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = ErrorBody {
            error: self.0.to_string(),
        };
        (status, Json(body)).into_response()
    }
}
