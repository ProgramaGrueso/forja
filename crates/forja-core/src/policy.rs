use crate::error::{ForjaError, Result};
use crate::types::{AgentKind, Privacy};

/// Devuelve los agentes permitidos para el nivel de privacidad indicado.
///
/// `Open` permite todos los agentes conocidos; `Sensitive` solo permite
/// `Claude`.
pub fn allowed_agents(privacy: Privacy) -> Vec<AgentKind> {
    match privacy {
        Privacy::Sensitive => vec![AgentKind::Claude],
        Privacy::Open => vec![
            AgentKind::Agy,
            AgentKind::Mock,
            AgentKind::Claude,
            AgentKind::Opencode,
            AgentKind::Hermes,
            AgentKind::Local,
        ],
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
    fn policy_sensitive_only_returns_claude() {
        let allowed = allowed_agents(Privacy::Sensitive);
        assert_eq!(allowed, vec![AgentKind::Claude]);
        for denied in [
            AgentKind::Mock,
            AgentKind::Agy,
            AgentKind::Opencode,
            AgentKind::Hermes,
            AgentKind::Local,
        ] {
            assert!(
                !allowed.contains(&denied),
                "sensitive no debe permitir {denied}"
            );
        }
    }

    #[test]
    fn policy_open_allows_every_agent() {
        let allowed = allowed_agents(Privacy::Open);
        assert_eq!(
            allowed,
            vec![
                AgentKind::Agy,
                AgentKind::Mock,
                AgentKind::Claude,
                AgentKind::Opencode,
                AgentKind::Hermes,
                AgentKind::Local,
            ]
        );
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
    fn policy_check_denies_sensitive_with_new_agents() {
        for agent in [AgentKind::Opencode, AgentKind::Hermes, AgentKind::Local] {
            let err = check_policy(Privacy::Sensitive, agent).unwrap_err();
            assert!(
                matches!(
                    err,
                    ForjaError::PolicyDenied {
                        privacy: Privacy::Sensitive,
                        agent: denied
                    } if denied == agent
                ),
                "esperaba PolicyDenied para {agent}, obtuve: {err:?}"
            );
        }
    }

    #[test]
    fn policy_check_allows_valid_combinations() {
        assert!(check_policy(Privacy::Sensitive, AgentKind::Claude).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Agy).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Mock).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Claude).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Opencode).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Hermes).is_ok());
        assert!(check_policy(Privacy::Open, AgentKind::Local).is_ok());
    }
}
