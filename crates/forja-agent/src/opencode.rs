use forja_core::{AgentKind, ForjaError};

use crate::{Agent, AgentFuture, RunContext};

/// Modelo por defecto del adaptador OpenCode.
pub const DEFAULT_MODEL: &str = "opencode-go/deepseek-v4.1-flash";

/// Adaptador stub para OpenCode; devuelve `ForjaError::NotConfigured` hasta que se configure.
#[derive(Debug, Clone)]
pub struct OpencodeAgent {
    pub model: String,
}

impl Default for OpencodeAgent {
    fn default() -> Self {
        Self::new(DEFAULT_MODEL)
    }
}

impl OpencodeAgent {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

impl Agent for OpencodeAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Opencode
    }

    fn run<'a>(&'a self, _ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            Err(ForjaError::NotConfigured(
                "adaptador opencode aún no configurado".to_string(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use forja_core::{AgentKind, ForjaError, Privacy, Task};

    use super::*;

    #[test]
    fn opencode_agent_keeps_model_with_default() {
        let agent = OpencodeAgent::default();
        assert_eq!(agent.model, DEFAULT_MODEL);
        assert_eq!(agent.model, "opencode-go/deepseek-v4.1-flash");
        assert_eq!(OpencodeAgent::new("otro/modelo").model, "otro/modelo");
    }

    #[tokio::test]
    async fn opencode_stub_returns_not_configured() {
        let agent = OpencodeAgent::default();
        assert_eq!(agent.kind(), AgentKind::Opencode);

        let ctx = RunContext {
            task: Task::new("Abierta", "Ejecuta algo", Privacy::Open),
            worktree: PathBuf::from("/tmp"),
            timeout_secs: 30,
        };

        let err = agent
            .run(&ctx)
            .await
            .expect_err("OpencodeAgent stub debe devolver NotConfigured");
        assert!(
            matches!(err, ForjaError::NotConfigured(_)),
            "esperaba ForjaError::NotConfigured, obtuve: {err:?}"
        );
    }
}
