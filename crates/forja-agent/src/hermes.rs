use forja_core::{AgentKind, ForjaError};

use crate::{Agent, AgentFuture, RunContext};

/// Perfil por defecto del adaptador Hermes.
pub const DEFAULT_PROFILE: &str = "yuuki";

/// Adaptador stub para Hermes; devuelve `ForjaError::NotConfigured` hasta que se configure.
#[derive(Debug, Clone)]
pub struct HermesAgent {
    pub profile: String,
}

impl Default for HermesAgent {
    fn default() -> Self {
        Self::new(DEFAULT_PROFILE)
    }
}

impl HermesAgent {
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
        }
    }
}

impl Agent for HermesAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Hermes
    }

    fn run<'a>(&'a self, _ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            Err(ForjaError::NotConfigured(
                "adaptador hermes aún no configurado".to_string(),
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
    fn hermes_agent_keeps_profile_with_default() {
        let agent = HermesAgent::default();
        assert_eq!(agent.profile, DEFAULT_PROFILE);
        assert_eq!(agent.profile, "yuuki");
        assert_eq!(HermesAgent::new("otro-perfil").profile, "otro-perfil");
    }

    #[tokio::test]
    async fn hermes_stub_returns_not_configured() {
        let agent = HermesAgent::default();
        assert_eq!(agent.kind(), AgentKind::Hermes);

        let ctx = RunContext {
            task: Task::new("Abierta", "Ejecuta algo", Privacy::Open),
            worktree: PathBuf::from("/tmp"),
            timeout_secs: 30,
        };

        let err = agent
            .run(&ctx)
            .await
            .expect_err("HermesAgent stub debe devolver NotConfigured");
        assert!(
            matches!(err, ForjaError::NotConfigured(_)),
            "esperaba ForjaError::NotConfigured, obtuve: {err:?}"
        );
    }
}
