use forja_core::{AgentKind, ForjaError};

use crate::{Agent, AgentFuture, RunContext};

/// Modelo local por defecto del adaptador Local.
pub const DEFAULT_MODEL: &str = "moe";

/// Adaptador stub para modelos locales; devuelve `ForjaError::NotConfigured` hasta que se configure.
#[derive(Debug, Clone)]
pub struct LocalAgent {
    pub model: String,
}

impl Default for LocalAgent {
    fn default() -> Self {
        Self::new(DEFAULT_MODEL)
    }
}

impl LocalAgent {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

impl Agent for LocalAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Local
    }

    fn run<'a>(&'a self, _ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            Err(ForjaError::NotConfigured(
                "adaptador local aún no configurado".to_string(),
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
    fn local_agent_keeps_model_with_default() {
        let agent = LocalAgent::default();
        assert_eq!(agent.model, DEFAULT_MODEL);
        assert_eq!(agent.model, "moe");
        assert_eq!(LocalAgent::new("otro-modelo").model, "otro-modelo");
    }

    #[tokio::test]
    async fn local_stub_returns_not_configured() {
        let agent = LocalAgent::default();
        assert_eq!(agent.kind(), AgentKind::Local);

        let ctx = RunContext {
            task: Task::new("Abierta", "Ejecuta algo", Privacy::Open),
            worktree: PathBuf::from("/tmp"),
            timeout_secs: 30,
        };

        let err = agent
            .run(&ctx)
            .await
            .expect_err("LocalAgent stub debe devolver NotConfigured");
        assert!(
            matches!(err, ForjaError::NotConfigured(_)),
            "esperaba ForjaError::NotConfigured, obtuve: {err:?}"
        );
    }
}
