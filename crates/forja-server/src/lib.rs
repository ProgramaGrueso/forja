pub mod api;
pub mod conversation_store;
pub mod delegation;
pub mod files;
mod guard;
pub mod models;
pub mod orchestrator;
pub mod quota;
pub mod reactor;
pub mod review;
mod security;
pub mod settings;
pub mod telemetry;
pub mod workspaces;

mod handlers;
mod router;
mod state;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;

use std::future::IntoFuture;

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
    let scheduler = state.scheduler.clone();
    let app = router::build_router_for_port(state, port);
    // Sin apagado "graceful" de axum: las conexiones SSE (/events) no terminan
    // nunca y lo bloquearían. Al recibir la señal se deja de servir y se cierran
    // las ejecuciones en curso.
    let (res, reason) = tokio::select! {
        res = axum::serve(listener, app).into_future() => (res, "servidor detenido".to_string()),
        sig = shutdown_signal() => {
            eprintln!("forja: recibida {sig}; deteniendo las ejecuciones en curso…");
            (Ok(()), format!("servidor detenido ({sig}); la ejecución se canceló"))
        }
    };
    sampler.abort();
    scheduler.shutdown(&reason).await;
    res?;
    Ok(())
}

/// Espera SIGTERM (`forja app --stop`, systemd), SIGINT (Ctrl+C) o SIGHUP
/// (terminal cerrada) y devuelve su nombre. Los agentes viven en su propio grupo
/// de procesos y no reciben esas señales: hay que cerrarlos explícitamente.
pub async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
            signal(SignalKind::hangup()),
        ) else {
            std::future::pending::<()>().await;
            unreachable!();
        };
        tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
            _ = hup.recv() => "SIGHUP",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "Ctrl+C"
    }
}
