use crate::error::{ForjaError, Result};
use crate::types::{AgentKind, Privacy};

/// Devuelve los agentes permitidos para el nivel de privacidad indicado.
pub fn allowed_agents(privacy: Privacy) -> Vec<AgentKind> {
    match privacy {
        Privacy::Sensitive => vec![AgentKind::Claude],
        Privacy::Open => vec![AgentKind::Agy, AgentKind::Mock, AgentKind::Claude],
    }
}

/// Indica si un agente está permitido bajo la política de privacidad dada.
pub fn is_agent_allowed(privacy: Privacy, agent: AgentKind) -> bool {
    allowed_agents(privacy).contains(&agent)
}

/// Valida que el agente pedido cumple la política de privacidad; de lo contrario devuelve `PolicyDenied`.
pub fn check_policy(privacy: Privacy, agent: AgentKind) -> Result<()> {
    if is_agent_allowed(privacy, agent) {
        Ok(())
    } else {
        Err(ForjaError::PolicyDenied { privacy, agent })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_sensitive_never_returns_agy() {
        let allowed = allowed_agents(Privacy::Sensitive);
        assert_eq!(allowed, vec![AgentKind::Claude]);
        assert!(!allowed.contains(&AgentKind::Agy));
        assert!(!allowed.contains(&AgentKind::Mock));
    }

    #[test]
    fn policy_open_allows_agy_mock_and_claude() {
        let allowed = allowed_agents(Privacy::Open);
        assert!(allowed.contains(&AgentKind::Agy));
        assert!(allowed.contains(&AgentKind::Mock));
        assert!(allowed.contains(&AgentKind::Claude));
        assert_eq!(allowed.len(), 3);
    }

    #[test]
    fn policy_check_denies_sensitive_with_agy() {
        let err = check_policy(Privacy::Sensitive, AgentKind::Agy).unwrap_err();
        assert!(matches!(
            err,
            ForjaError::PolicyDenied {
                privacy: Privacy::Sensitive,
                agent: AgentKind::Agy
            }
        ));
    }

    #[test]
    fn policy_check_allows_valid_combinations() {
        assert!(check_policy(Privacy::Sensitive, AgentKind::Claude).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Agy).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Mock).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Claude).is_ok());
    }
}
