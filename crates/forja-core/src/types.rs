use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ForjaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    Open,
    Sensitive,
}

impl fmt::Display for Privacy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::Sensitive => write!(f, "sensitive"),
        }
    }
}

impl FromStr for Privacy {
    type Err = ForjaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Ok(Self::Open),
            "sensitive" => Ok(Self::Sensitive),
            other => Err(ForjaError::InvalidInput(format!(
                "privacidad desconocida: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Mock,
    Agy,
    Claude,
    Opencode,
    Hermes,
    Local,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mock => write!(f, "mock"),
            Self::Agy => write!(f, "agy"),
            Self::Claude => write!(f, "claude"),
            Self::Opencode => write!(f, "opencode"),
            Self::Hermes => write!(f, "hermes"),
            Self::Local => write!(f, "local"),
        }
    }
}

impl FromStr for AgentKind {
    type Err = ForjaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "agy" => Ok(Self::Agy),
            "claude" => Ok(Self::Claude),
            "opencode" => Ok(Self::Opencode),
            "hermes" => Ok(Self::Hermes),
            "local" => Ok(Self::Local),
            other => Err(ForjaError::InvalidInput(format!(
                "agente desconocido: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub titulo: String,
    pub prompt: String,
    pub privacidad: Privacy,
}

impl Task {
    pub fn new(titulo: impl Into<String>, prompt: impl Into<String>, privacidad: Privacy) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            titulo: titulo.into(),
            prompt: prompt.into(),
            privacidad,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub task: Task,
    pub agent: AgentKind,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
