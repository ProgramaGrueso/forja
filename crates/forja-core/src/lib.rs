pub mod error;
pub mod policy;
pub mod types;

pub use error::{ForjaError, Result};
pub use policy::{allowed_agents, check_policy, is_agent_allowed};
pub use types::{AgentKind, Privacy, RunRecord, RunStatus, Task};

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn serde_roundtrip_task() {
        let task = Task {
            id: "task-001".to_string(),
            titulo: "Implementar parser".to_string(),
            prompt: "Escribe un parser tolerante".to_string(),
            privacidad: Privacy::Sensitive,
        };
        let json = serde_json::to_string(&task).expect("serialize task");
        let parsed: Task = serde_json::from_str(&json).expect("deserialize task");
        assert_eq!(task, parsed);
    }

    #[test]
    fn serde_roundtrip_privacy() {
        for privacy in [Privacy::Open, Privacy::Sensitive] {
            let json = serde_json::to_string(&privacy).expect("serialize privacy");
            let parsed: Privacy = serde_json::from_str(&json).expect("deserialize privacy");
            assert_eq!(privacy, parsed);
        }
        assert_eq!(serde_json::to_string(&Privacy::Open).unwrap(), "\"open\"");
        assert_eq!(
            serde_json::to_string(&Privacy::Sensitive).unwrap(),
            "\"sensitive\""
        );
    }

    #[test]
    fn serde_roundtrip_agent_kind() {
        for agent in [
            AgentKind::Mock,
            AgentKind::Agy,
            AgentKind::Claude,
            AgentKind::Opencode,
            AgentKind::Hermes,
            AgentKind::Local,
        ] {
            let json = serde_json::to_string(&agent).expect("serialize agent");
            let parsed: AgentKind = serde_json::from_str(&json).expect("deserialize agent");
            assert_eq!(agent, parsed);
        }
        // Serde en minúsculas para todas las variantes.
        assert_eq!(serde_json::to_string(&AgentKind::Mock).unwrap(), "\"mock\"");
        assert_eq!(serde_json::to_string(&AgentKind::Agy).unwrap(), "\"agy\"");
        assert_eq!(
            serde_json::to_string(&AgentKind::Claude).unwrap(),
            "\"claude\""
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Opencode).unwrap(),
            "\"opencode\""
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Hermes).unwrap(),
            "\"hermes\""
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Local).unwrap(),
            "\"local\""
        );
    }

    #[test]
    fn serde_roundtrip_run_status() {
        for status in [
            RunStatus::Pending,
            RunStatus::Running,
            RunStatus::Done,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            let json = serde_json::to_string(&status).expect("serialize status");
            let parsed: RunStatus = serde_json::from_str(&json).expect("deserialize status");
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn serde_roundtrip_run_record() {
        let record = RunRecord {
            id: "run-1".to_string(),
            task: Task::new("Prueba", "Haz algo", Privacy::Open),
            agent: AgentKind::Mock,
            status: RunStatus::Done,
            branch: Some("forja/run-1".to_string()),
            worktree: Some("/tmp/wt".to_string()),
            attempts: 1,
            output: Some("ok".to_string()),
            error: None,
            created_at_ms: 1000,
            updated_at_ms: 2000,
        };
        let json = serde_json::to_string(&record).expect("serialize run record");
        let parsed: RunRecord = serde_json::from_str(&json).expect("deserialize run record");
        assert_eq!(record, parsed);
    }

    #[test]
    fn display_forja_errors() {
        let denied = ForjaError::PolicyDenied {
            privacy: Privacy::Sensitive,
            agent: AgentKind::Agy,
        };
        assert!(denied.to_string().contains("agy"));
        assert!(denied.to_string().contains("sensitive"));

        let timeout = ForjaError::Timeout { seconds: 30 };
        assert!(timeout.to_string().contains("30s"));

        let git = ForjaError::Git("rama duplicada".to_string());
        assert!(git.to_string().contains("rama duplicada"));

        let not_cfg = ForjaError::NotConfigured("claude".to_string());
        assert!(not_cfg.to_string().contains("claude"));
    }

    #[test]
    fn parse_privacy_and_agent_from_str() {
        assert_eq!(Privacy::from_str("open").unwrap(), Privacy::Open);
        assert_eq!(Privacy::from_str("SENSITIVE").unwrap(), Privacy::Sensitive);
        assert!(Privacy::from_str("secreto").is_err());

        assert_eq!(AgentKind::from_str("mock").unwrap(), AgentKind::Mock);
        assert_eq!(AgentKind::from_str("agy").unwrap(), AgentKind::Agy);
        assert_eq!(AgentKind::from_str("claude").unwrap(), AgentKind::Claude);
        assert_eq!(
            AgentKind::from_str("opencode").unwrap(),
            AgentKind::Opencode
        );
        assert_eq!(AgentKind::from_str("hermes").unwrap(), AgentKind::Hermes);
        assert_eq!(AgentKind::from_str("local").unwrap(), AgentKind::Local);
        assert!(AgentKind::from_str("otro").is_err());

        // Display y FromStr coherentes para todas las variantes.
        for agent in [
            AgentKind::Mock,
            AgentKind::Agy,
            AgentKind::Claude,
            AgentKind::Opencode,
            AgentKind::Hermes,
            AgentKind::Local,
        ] {
            let text = agent.to_string();
            assert_eq!(AgentKind::from_str(&text).unwrap(), agent);
            assert_eq!(AgentKind::from_str(&text.to_uppercase()).unwrap(), agent);
        }
    }
}
