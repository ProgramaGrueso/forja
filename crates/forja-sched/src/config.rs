use std::path::PathBuf;
use std::time::Duration;

use forja_core::{default_sensitive_agents, AgentKind};

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub max_concurrency: usize,
    pub timeout_secs: u64,
    pub worktrees_dir: PathBuf,
    pub repo_path: Option<PathBuf>,
    pub base_branch: String,
    pub max_retries: u32,
    pub retry_backoff: Duration,
    pub sensitive_agents: Vec<AgentKind>,
}

impl SchedulerConfig {
    pub fn new(worktrees_dir: impl Into<PathBuf>) -> Self {
        Self {
            max_concurrency: 2,
            timeout_secs: 300,
            worktrees_dir: worktrees_dir.into(),
            repo_path: None,
            base_branch: "main".to_string(),
            max_retries: 2,
            retry_backoff: Duration::from_millis(20),
            sensitive_agents: default_sensitive_agents(),
        }
    }
}
