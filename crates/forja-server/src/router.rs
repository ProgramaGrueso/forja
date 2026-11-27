use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::api;
use crate::handlers::{
    cancel_run, create_task, events_handler, get_run, list_runs, mascot_handler, panel_handler,
    run_output, run_stream, ErrorBody,
};
use crate::orchestrator;
use crate::review;
use crate::security;
use crate::state::AppState;

pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Construye el router con el puerto real en el que escucha el servidor
/// (relevante si se vincula un puerto efímero). El puerto se usa para
/// validar `Host`/`Origin` contra DNS rebinding y CSRF.
pub(crate) fn build_router_for_port(state: AppState, port: u16) -> Router {
    // Reacciona al fin de las tareas delegadas aunque el panel no esté mirando.
    state.ensure_run_watcher();
    let protected = Router::new()
        .route("/events", get(events_handler))
        .route("/tasks", post(create_task))
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/output", get(run_output))
        .route("/runs/{id}/stream", get(run_stream))
        .route("/runs/{id}/diff", get(review::get_diff))
        .route("/runs/{id}/merge", post(review::merge))
        .route("/runs/{id}/discard", post(review::discard))
        .route("/api/telemetry", get(api::get_telemetry))
        .route("/api/agents", get(api::agents))
        .route("/api/stats", get(api::stats))
        .route("/api/models", get(api::get_models))
        .route("/api/orchestrator/chat", post(orchestrator::chat))
        .route("/api/orchestrator/profile", get(api::orchestrator_profile))
        .route("/api/orchestrator/reset", post(orchestrator::reset))
        .route(
            "/api/orchestrator/conversations",
            get(orchestrator::list_conversations).post(orchestrator::create_conversation),
        )
        .route(
            "/api/orchestrator/conversations/{id}",
            get(orchestrator::get_conversation).delete(orchestrator::delete_conversation),
        )
        .route("/api/policy", get(api::policy))
        .route("/api/quota", get(api::get_quota).put(api::put_quota))
        .route(
            "/api/settings",
            get(api::get_settings).put(api::put_settings),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/", get(panel_handler))
        .route("/api/health", get(api::health))
        .route("/mascot/{name}", get(mascot_handler))
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn(move |req, next| {
            security::guard(port, req, next)
        }))
        .with_state(state)
}

async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = state.auth_token.as_deref() else {
        return next.run(req).await;
    };

    let bearer_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .is_some_and(|tok| tok == expected);

    let query_ok = req.uri().query().is_some_and(|q| {
        q.split('&')
            .filter_map(|pair| pair.split_once('='))
            .any(|(k, v)| k == "token" && v == expected)
    });

    if bearer_ok || query_ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "autenticación requerida (Authorization: Bearer <token>)".to_string(),
            }),
        )
            .into_response()
    }
}
