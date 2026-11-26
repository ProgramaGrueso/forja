use forja_core::{AgentKind, ForjaError};

use crate::{Agent, AgentFuture, RunContext};

/// Adaptador stub para Claude; devuelve `ForjaError::NotConfigured` hasta que se configure.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeAgent;

impl ClaudeAgent {
    pub fn new() -> Self {
        Self
    }
}

impl Agent for ClaudeAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn run<'a>(&'a self, _ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            Err(ForjaError::NotConfigured(
                "adaptador claude aún no configurado".to_string(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use forja_core::{AgentKind, ForjaError, Privacy, Task};

    use super::*;

    #[tokio::test]
    async fn claude_stub_returns_not_configured() {
        let agent = ClaudeAgent::new();
        assert_eq!(agent.kind(), AgentKind::Claude);

        let ctx = RunContext {
            task: Task::new("Sensible", "Revisar secreto", Privacy::Sensitive),
            worktree: PathBuf::from("/tmp"),
            timeout_secs: 30,
        };

        let err = agent
            .run(&ctx)
            .await
            .expect_err("ClaudeAgent stub debe devolver NotConfigured");
        assert!(
            matches!(err, ForjaError::NotConfigured(_)),
            "esperaba ForjaError::NotConfigured, obtuve: {err:?}"
        );
    }
}
