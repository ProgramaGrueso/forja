use crate::error::{ForjaError, Result};
use crate::types::{AgentKind, Privacy};

/// Lista estricta por defecto de agentes permitidos para privacidad `Sensitive`.
pub fn default_sensitive_agents() -> Vec<AgentKind> {
    vec![AgentKind::Claude]
}

/// Devuelve los agentes permitidos para el nivel de privacidad indicado usando
/// la lista configurada para `Sensitive`.
///
/// `Open` permite todos los agentes conocidos; `Sensitive` solo permite los
/// agentes incluidos en `sensitive_agents`.
pub fn allowed_agents_with(privacy: Privacy, sensitive_agents: &[AgentKind]) -> Vec<AgentKind> {
    match privacy {
        Privacy::Sensitive => sensitive_agents.to_vec(),
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

/// Devuelve los agentes permitidos para el nivel de privacidad indicado con la
/// política por defecto (`Sensitive` solo permite `Claude`).
pub fn allowed_agents(privacy: Privacy) -> Vec<AgentKind> {
    allowed_agents_with(privacy, &default_sensitive_agents())
}

/// Indica si un agente está permitido bajo la política de privacidad dada y la
/// lista de agentes permitidos para `Sensitive`.
pub fn is_agent_allowed_with(
    privacy: Privacy,
    agent: AgentKind,
    sensitive_agents: &[AgentKind],
) -> bool {
    allowed_agents_with(privacy, sensitive_agents).contains(&agent)
}

/// Indica si un agente está permitido bajo la política de privacidad por defecto.
pub fn is_agent_allowed(privacy: Privacy, agent: AgentKind) -> bool {
    is_agent_allowed_with(privacy, agent, &default_sensitive_agents())
}

/// Valida que el agente pedido cumple la política de privacidad usando la lista
/// configurada para `Sensitive`; de lo contrario devuelve `PolicyDenied`.
pub fn check_policy_with(
    privacy: Privacy,
    agent: AgentKind,
    sensitive_agents: &[AgentKind],
) -> Result<()> {
    if is_agent_allowed_with(privacy, agent, sensitive_agents) {
        Ok(())
    } else {
        Err(ForjaError::PolicyDenied { privacy, agent })
    }
}

/// Valida que el agente pedido cumple la política de privacidad por defecto; de
/// lo contrario devuelve `PolicyDenied`.
pub fn check_policy(privacy: Privacy, agent: AgentKind) -> Result<()> {
    check_policy_with(privacy, agent, &default_sensitive_agents())
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

    #[test]
    fn policy_check_with_custom_sensitive_list_local_only() {
        let allowed = [AgentKind::Local];
        assert_eq!(
            allowed_agents_with(Privacy::Sensitive, &allowed),
            vec![AgentKind::Local]
        );
        assert!(check_policy_with(Privacy::Sensitive, AgentKind::Local, &allowed).is_ok());
        assert!(matches!(
            check_policy_with(Privacy::Sensitive, AgentKind::Agy, &allowed),
            Err(ForjaError::PolicyDenied {
                privacy: Privacy::Sensitive,
                agent: AgentKind::Agy
            })
        ));
        assert!(matches!(
            check_policy_with(Privacy::Sensitive, AgentKind::Claude, &allowed),
            Err(ForjaError::PolicyDenied {
                privacy: Privacy::Sensitive,
                agent: AgentKind::Claude
            })
        ));
        // Open siempre permite todos los agentes sin depender de sensitive_agents.
        assert!(check_policy_with(Privacy::Open, AgentKind::Agy, &allowed).is_ok());
    }
}
