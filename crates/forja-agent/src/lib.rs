pub mod agy;
pub mod claude;
pub mod hermes;
pub mod local;
pub mod mock;
pub mod opencode;
pub mod runner;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use forja_core::{AgentKind, Result, Task};
use serde::{Deserialize, Serialize};

pub use agy::{build_command, parse_agy_output, AgyAgent, AgyCommandOptions};
pub use claude::ClaudeAgent;
pub use hermes::HermesAgent;
pub use local::{build_command as build_local_command, LocalAgent, LocalCommandOptions};
pub use mock::{MockAgent, MockBehavior, MockFileWrite};
pub use opencode::{
    build_command as build_opencode_command, parse_opencode_output, OpencodeAgent,
    OpencodeCommandOptions,
};
pub use runner::{CommandResult, Runner};

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
    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a>;
}
