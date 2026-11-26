pub mod agy;
pub mod claude;
pub mod hermes;
pub mod local;
pub mod mock;
pub mod models;
pub mod opencode;
pub mod runner;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use forja_core::{AgentKind, Result, Task};
use serde::{Deserialize, Serialize};

pub use agy::{build_command, parse_agy_output, AgyAgent, AgyCommandOptions};
pub use claude::ClaudeAgent;
pub use hermes::HermesAgent;
pub use local::{build_command as build_local_command, LocalAgent, LocalCommandOptions};
pub use mock::{MockAgent, MockBehavior, MockFileWrite};
pub use models::{
    build_agent, default_model, effort_levels, opencode_model_with_effort, resolve_effort,
    resolve_model, validate_effort, validate_model, EFFORT_AGENTS, MODEL_AGENTS,
};
pub use opencode::{
    build_command as build_opencode_command, parse_opencode_output, OpencodeAgent,
    OpencodeCommandOptions,
};
pub use runner::{with_output_sink, CommandResult, OutputSink, OutputStream, Runner};

#[derive(Debug, Clone)]
pub struct RunContext {
    pub task: Task,
    pub worktree: PathBuf,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOutput {
    pub text: String,
    pub raw_stdout: String,
    pub raw_stderr: String,
    pub exit_code: Option<i32>,
    pub warnings: Vec<String>,
}

pub type AgentFuture<'a> = Pin<Box<dyn Future<Output = Result<AgentOutput>> + Send + 'a>>;

pub trait Agent: Send + Sync {
    fn kind(&self) -> AgentKind;

    /// Modelo efectivo del agente (None si no aplica: mock, hermes).
    fn model(&self) -> Option<String> {
        None
    }

    /// Copia del agente con otro modelo, conservando el resto de su configuración.
    /// None si el agente no admite elegir modelo.
    fn with_model(&self, _model: &str) -> Option<Arc<dyn Agent>> {
        None
    }

    /// Esfuerzo de razonamiento configurado (None si no se fijó o no aplica).
    fn effort(&self) -> Option<String> {
        None
    }

    /// Copia del agente con otro esfuerzo de razonamiento. None si el agente no lo admite.
    fn with_effort(&self, _effort: &str) -> Option<Arc<dyn Agent>> {
        None
    }

    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a>;
}
