use std::time::Duration;

use forja_core::{AgentKind, ForjaError};

use crate::{Agent, AgentFuture, AgentOutput, RunContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockBehavior {
    Success { text: String },
    Fail { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockFileWrite {
    pub filename: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockAgent {
    pub behavior: MockBehavior,
    pub delay: Duration,
    pub file_write: Option<MockFileWrite>,
}

impl Default for MockAgent {
    fn default() -> Self {
        Self::success("mock ok")
    }
}

impl MockAgent {
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            behavior: MockBehavior::Success { text: text.into() },
            delay: Duration::ZERO,
            file_write: None,
        }
    }

    pub fn fail(message: impl Into<String>) -> Self {
        Self {
            behavior: MockBehavior::Fail {
                message: message.into(),
            },
            delay: Duration::ZERO,
            file_write: None,
        }
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn with_file(mut self, filename: impl Into<String>, content: impl Into<String>) -> Self {
        self.file_write = Some(MockFileWrite {
            filename: Some(filename.into()),
            content: content.into(),
        });
        self
    }

    pub fn with_default_file(mut self, content: impl Into<String>) -> Self {
        self.file_write = Some(MockFileWrite {
            filename: None,
            content: content.into(),
        });
        self
    }
}

impl Agent for MockAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Mock
    }

    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            let timeout = Duration::from_secs(ctx.timeout_secs);
            let work = async {
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }

                match &self.behavior {
                    MockBehavior::Fail { message } => Err(ForjaError::Agent(message.clone())),
                    MockBehavior::Success { text } => {
                        if let Some(fw) = &self.file_write {
                            let name = fw
                                .filename
                                .clone()
                                .unwrap_or_else(|| format!("mock-{}.txt", ctx.task.id));
                            let target = ctx.worktree.join(name);
                            if let Some(parent) = target.parent() {
                                tokio::fs::create_dir_all(parent).await?;
                            }
                            tokio::fs::write(&target, &fw.content).await?;
                        }

                        Ok(AgentOutput {
                            text: text.clone(),
                            raw_stdout: text.clone(),
                            raw_stderr: String::new(),
                            exit_code: Some(0),
                            warnings: Vec::new(),
                        })
                    }
                }
            };

            match tokio::time::timeout(timeout, work).await {
                Ok(res) => res,
                Err(_) => Err(ForjaError::Timeout {
                    seconds: ctx.timeout_secs,
                }),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use forja_core::{AgentKind, ForjaError, Privacy, Task};
    use tempfile::tempdir;

    use super::*;

    fn sample_context(worktree: &std::path::Path, timeout_secs: u64) -> RunContext {
        RunContext {
            task: Task {
                id: "task-mock-1".to_string(),
                titulo: "Tarea de prueba".to_string(),
                prompt: "Haz algo".to_string(),
                privacidad: Privacy::Open,
            },
            worktree: worktree.to_path_buf(),
            timeout_secs,
        }
    }

    #[tokio::test]
    async fn mock_agent_success_writes_file() {
        let dir = tempdir().expect("tempdir");
        let ctx = sample_context(dir.path(), 5);
        let agent: Arc<dyn Agent> =
            Arc::new(MockAgent::success("todo listo").with_file("resultado.txt", "contenido mock"));

        assert_eq!(agent.kind(), AgentKind::Mock);
        let output = agent.run(&ctx).await.expect("mock success");
        assert_eq!(output.text, "todo listo");
        assert_eq!(output.raw_stdout, "todo listo");
        assert_eq!(output.raw_stderr, "");
        assert_eq!(output.exit_code, Some(0));
        assert!(output.warnings.is_empty());

        let written = std::fs::read_to_string(dir.path().join("resultado.txt"))
            .expect("archivo escrito en worktree");
        assert_eq!(written, "contenido mock");
    }

    #[tokio::test]
    async fn mock_agent_success_default_filename() {
        let dir = tempdir().expect("tempdir");
        let ctx = sample_context(dir.path(), 5);
        let agent = MockAgent::success("ok").with_default_file("por defecto");

        let output = agent.run(&ctx).await.expect("mock success");
        assert_eq!(output.text, "ok");

        let written = std::fs::read_to_string(dir.path().join("mock-task-mock-1.txt"))
            .expect("archivo por defecto escrito");
        assert_eq!(written, "por defecto");
    }

    #[tokio::test]
    async fn mock_agent_fail_returns_agent_error() {
        let dir = tempdir().expect("tempdir");
        let ctx = sample_context(dir.path(), 5);
        let agent = MockAgent::fail("fallo simulado");

        let err = agent.run(&ctx).await.expect_err("mock fail");
        match err {
            ForjaError::Agent(msg) => assert_eq!(msg, "fallo simulado"),
            other => panic!("esperaba ForjaError::Agent, obtuve: {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_agent_timeout_returns_timeout_error() {
        let dir = tempdir().expect("tempdir");
        let ctx = sample_context(dir.path(), 1);
        let agent = MockAgent::success("tarde").with_delay(Duration::from_millis(1200));

        let err = agent.run(&ctx).await.expect_err("mock timeout");
        match err {
            ForjaError::Timeout { seconds } => assert_eq!(seconds, 1),
            other => panic!("esperaba ForjaError::Timeout, obtuve: {other:?}"),
        }
    }
}
