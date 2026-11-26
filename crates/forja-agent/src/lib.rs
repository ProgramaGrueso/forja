pub mod agy;
pub mod mock;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use forja_core::{AgentKind, Result, Task};
use serde::{Deserialize, Serialize};

pub use agy::{build_command, AgyCommandOptions};
pub use mock::{MockAgent, MockBehavior, MockFileWrite};

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
