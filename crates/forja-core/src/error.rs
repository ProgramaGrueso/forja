use thiserror::Error;

use crate::types::{AgentKind, Privacy};

#[derive(Debug, Error)]
pub enum ForjaError {
    #[error(
        "política de privacidad denegada: agente {agent} no permitido para privacidad {privacy}"
    )]
    PolicyDenied { privacy: Privacy, agent: AgentKind },

    #[error("error de git: {0}")]
    Git(String),

    #[error("error de agente: {0}")]
    Agent(String),

    #[error("tiempo de espera agotado tras {seconds}s")]
    Timeout { seconds: u64 },

    #[error("error de almacenamiento: {0}")]
    Store(String),

    #[error("recurso no encontrado: {0}")]
    NotFound(String),

    #[error("no configurado: {0}")]
    NotConfigured(String),

    #[error("ejecución cancelada: {0}")]
    Cancelled(String),

    #[error("entrada inválida: {0}")]
    InvalidInput(String),

    #[error("error de E/S: {0}")]
    Io(#[from] std::io::Error),

    #[error("error de serialización JSON: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ForjaError>;
