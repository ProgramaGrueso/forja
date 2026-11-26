//! Resolución de modelos por agente: override por tarea > ajustes > default en código.

use std::collections::BTreeMap;
use std::sync::Arc;

use forja_core::{AgentKind, ForjaError, Result};

use crate::{Agent, AgyAgent, ClaudeAgent, HermesAgent, LocalAgent, MockAgent, OpencodeAgent};

pub const MAX_MODEL_LEN: usize = 100;

/// Agentes cuyo modelo es configurable (mock y hermes no lo tienen).
pub const MODEL_AGENTS: [AgentKind; 4] = [
    AgentKind::Claude,
    AgentKind::Agy,
    AgentKind::Opencode,
    AgentKind::Local,
];

/// Modelo por defecto fijado en código (None si el agente no admite modelo).
pub fn default_model(kind: AgentKind) -> Option<&'static str> {
    match kind {
        AgentKind::Claude => Some(crate::claude::DEFAULT_MODEL),
        AgentKind::Agy => Some(crate::agy::DEFAULT_MODEL),
        AgentKind::Opencode => Some(crate::opencode::DEFAULT_MODEL),
        AgentKind::Local => Some(crate::local::DEFAULT_MODEL),
        AgentKind::Mock | AgentKind::Hermes => None,
    }
}

/// Valida y normaliza (trim) un nombre de modelo: no vacío, sin caracteres de
/// control, <= 100 caracteres y sin empezar por `-` (evita inyectar flags en argv).
pub fn validate_model(raw: &str) -> Result<String> {
    let model = raw.trim();
    if model.is_empty() {
        return Err(ForjaError::InvalidInput(
            "el modelo no puede estar vacío".to_string(),
        ));
    }
    if model.chars().count() > MAX_MODEL_LEN {
        return Err(ForjaError::InvalidInput(format!(
            "el modelo supera {MAX_MODEL_LEN} caracteres"
        )));
    }
    if model.chars().any(char::is_control) {
        return Err(ForjaError::InvalidInput(
            "el modelo contiene caracteres de control".to_string(),
        ));
    }
    if model.starts_with('-') {
        return Err(ForjaError::InvalidInput(
            "el modelo no puede empezar por '-'".to_string(),
        ));
    }
    Ok(model.to_string())
}

/// Prioridad: override por tarea > ajustes > default en código.
/// Devuelve None si el agente no admite modelo.
pub fn resolve_model(
    kind: AgentKind,
    task_override: Option<&str>,
    settings: &BTreeMap<String, String>,
) -> Option<String> {
    default_model(kind)?;
    let nonblank = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
    task_override
        .and_then(nonblank)
        .or_else(|| {
            settings
                .get(&kind.to_string())
                .map(String::as_str)
                .and_then(nonblank)
        })
        .or_else(|| default_model(kind).map(str::to_string))
}

/// Construye el agente de `kind` ya con su modelo efectivo resuelto.
pub fn build_agent(
    kind: AgentKind,
    task_override: Option<&str>,
    settings: &BTreeMap<String, String>,
) -> Arc<dyn Agent> {
    let model = resolve_model(kind, task_override, settings);
    match (kind, model) {
        (AgentKind::Claude, Some(m)) => Arc::new(ClaudeAgent::default().with_model(m)),
        (AgentKind::Agy, Some(m)) => Arc::new(AgyAgent::new(m, crate::agy::DEFAULT_MODE)),
        (AgentKind::Opencode, Some(m)) => Arc::new(OpencodeAgent::new(m)),
        (AgentKind::Local, Some(m)) => Arc::new(LocalAgent::new(m)),
        (AgentKind::Hermes, _) => Arc::new(HermesAgent::default()),
        _ => Arc::new(MockAgent::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn resolve_priority_task_over_settings_over_default() {
        let s = map(&[("claude", "opus")]);
        assert_eq!(
            resolve_model(AgentKind::Claude, Some("haiku"), &s).as_deref(),
            Some("haiku")
        );
        assert_eq!(
            resolve_model(AgentKind::Claude, None, &s).as_deref(),
            Some("opus")
        );
        assert_eq!(
            resolve_model(AgentKind::Agy, None, &s).as_deref(),
            Some("gemini-4-neon-medium")
        );
        // Override en blanco o ajuste en blanco se ignoran.
        let blank = map(&[("local", "  ")]);
        assert_eq!(
            resolve_model(AgentKind::Local, Some(" "), &blank).as_deref(),
            Some("moe")
        );
        assert_eq!(resolve_model(AgentKind::Mock, Some("x"), &s), None);
        assert_eq!(resolve_model(AgentKind::Hermes, None, &s), None);
    }

    #[test]
    fn validate_model_rules() {
        assert_eq!(validate_model("  sonnet ").unwrap(), "sonnet");
        assert!(validate_model("opencode-go/deepseek-v4.1-flash").is_ok());
        assert!(validate_model("").is_err());
        assert!(validate_model("   ").is_err());
        assert!(validate_model("a\nb").is_err());
        assert!(validate_model("a\tb").is_err());
        assert!(validate_model("--dangerously").is_err());
        assert!(validate_model(&"x".repeat(100)).is_ok());
        assert!(validate_model(&"x".repeat(101)).is_err());
    }

    #[test]
    fn build_agent_applies_model_for_each_agent() {
        let s = map(&[("claude", "opus"), ("agy", "g-x"), ("opencode", "oc/y")]);
        let model = |k, o: Option<&str>| build_agent(k, o, &s).model();
        assert_eq!(model(AgentKind::Claude, None).as_deref(), Some("opus"));
        assert_eq!(model(AgentKind::Agy, None).as_deref(), Some("g-x"));
        assert_eq!(model(AgentKind::Opencode, None).as_deref(), Some("oc/y"));
        assert_eq!(model(AgentKind::Local, None).as_deref(), Some("moe"));
        assert_eq!(
            model(AgentKind::Local, Some("qwen")).as_deref(),
            Some("qwen")
        );
        assert_eq!(model(AgentKind::Mock, None), None);
        assert_eq!(model(AgentKind::Hermes, None), None);
    }

    #[test]
    fn with_model_keeps_kind_and_changes_model() {
        let base = ClaudeAgent::default();
        let swapped = Agent::with_model(&base, "haiku").expect("claude admite modelo");
        assert_eq!(swapped.kind(), AgentKind::Claude);
        assert_eq!(swapped.model().as_deref(), Some("haiku"));
        assert!(MockAgent::default().with_model("x").is_none());
        assert!(HermesAgent::default().with_model("x").is_none());
    }
}
