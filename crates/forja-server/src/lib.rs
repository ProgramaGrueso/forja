pub mod api;
pub mod conversation_store;
pub mod delegation;
pub mod models;
pub mod orchestrator;
pub mod quota;
pub mod reactor;
pub mod review;
mod security;
pub mod settings;
pub mod telemetry;

mod handlers;
mod router;
mod state;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;

use axum::Router;
use forja_core::Result;

pub(crate) use handlers::ApiError;
pub use handlers::{mascot_bytes, CreateTaskRequest};
pub use router::MAX_BODY_BYTES;
pub use state::AppState;

pub fn build_router(state: AppState) -> Router {
    let port = state
        .settings
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .port;
    router::build_router_for_port(state, port)
}

pub async fn serve(addr: SocketAddr, state: AppState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let port = listener.local_addr()?.port();
    let sampler = state.telemetry.spawn_sampler();
    let res = axum::serve(listener, router::build_router_for_port(state, port)).await;
    sampler.abort();
    res?;
    Ok(())
}
