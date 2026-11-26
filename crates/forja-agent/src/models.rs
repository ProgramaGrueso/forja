//! Resolución de modelos por agente: override por tarea > ajustes > default en código.

use std::collections::BTreeMap;
use std::sync::Arc;

use forja_core::{AgentKind, ForjaError, Result};

use crate::{Agent, AgyAgent, ClaudeAgent, HermesAgent, LocalAgent, MockAgent, OpencodeAgent};

pub const MAX_MODEL_LEN: usize = 100;

/// Agentes cuyo modelo es configurable (mock no lo tiene). En hermes el modelo es
/// opcional: sin él se usa el del perfil.
pub const MODEL_AGENTS: [AgentKind; 5] = [
    AgentKind::Claude,
    AgentKind::Agy,
    AgentKind::Opencode,
    AgentKind::Hermes,
    AgentKind::Local,
];

/// Agentes con esfuerzo de razonamiento configurable (local y mock no lo tienen).
pub const EFFORT_AGENTS: [AgentKind; 4] = [
    AgentKind::Claude,
    AgentKind::Agy,
    AgentKind::Opencode,
    AgentKind::Hermes,
];

const LEVELS_CLAUDE_AGY: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const LEVELS_HERMES: [&str; 8] = [
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];
/// opencode no tiene flag aparte: la variante del proveedor es texto libre; esta
/// lista es solo la sugerencia habitual para la interfaz.
const LEVELS_OPENCODE_HINT: [&str; 4] = ["low", "medium", "high", "max"];
pub const MAX_OPENCODE_VARIANT_LEN: usize = 20;

/// Niveles de esfuerzo de la familia (lista vacía = no soporta esfuerzo). Para
/// opencode son solo sugerencias: acepta cualquier `[a-z0-9_-]{1,20}`.
pub fn effort_levels(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::Claude | AgentKind::Agy => &LEVELS_CLAUDE_AGY,
        AgentKind::Hermes => &LEVELS_HERMES,
        AgentKind::Opencode => &LEVELS_OPENCODE_HINT,
        AgentKind::Local | AgentKind::Mock => &[],
    }
}

/// Traduce alias en español y etiquetas del panel (p. ej. "alto", "Muy alto", "Máx")
/// al valor canónico (`high`, `xhigh`, `max`...). Lo que no es alias se devuelve
/// recortado y en minúsculas, sin más cambios.
pub fn normalize_effort_alias(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    let key: String = lower
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' => 'u',
            '_' | '-' => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let canonical = match key.as_str() {
        "minimo" | "minima" => "minimal",
        "bajo" | "baja" => "low",
        "med" | "medio" | "media" => "medium",
        "alto" | "alta" => "high",
        "muy alto" | "muy alta" | "muyalto" | "extra alto" => "xhigh",
        "max" | "maximo" | "maxima" => "max",
        "ninguno" | "ninguna" => "none",
        _ => return lower,
    };
    canonical.to_string()
}

/// Etiqueta en español de un valor canónico (para mensajes de error).
fn effort_label_es(level: &str) -> &'static str {
    match level {
        "none" => "ninguno",
        "minimal" => "mínimo",
        "low" => "bajo",
        "medium" => "medio",
        "high" => "alto",
        "xhigh" => "muy alto",
        "max" => "máximo",
        "ultra" => "ultra",
        _ => "",
    }
}

/// Lista de valores válidos de la familia, con su nombre en español.
pub fn effort_levels_es(kind: AgentKind) -> String {
    effort_levels(kind)
        .iter()
        .map(|l| format!("{l} ({})", effort_label_es(l)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Valida y normaliza un esfuerzo para la familia `kind` (alias en español incluidos),
/// con su propio conjunto de valores. Devuelve siempre el valor canónico.
pub fn validate_effort(kind: AgentKind, raw: &str) -> Result<String> {
    if !EFFORT_AGENTS.contains(&kind) {
        return Err(ForjaError::InvalidInput(format!(
            "el agente {kind} no admite esfuerzo de razonamiento"
        )));
    }
    let effort = normalize_effort_alias(raw);
    if effort.is_empty() {
        return Err(ForjaError::InvalidInput(
            "el esfuerzo no puede estar vacío".to_string(),
        ));
    }
    if kind == AgentKind::Opencode {
        let ok = effort.chars().count() <= MAX_OPENCODE_VARIANT_LEN
            && effort
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if !ok {
            return Err(ForjaError::InvalidInput(format!(
                "esfuerzo \"{effort}\" no válido para opencode (usa [a-z0-9_-], máx. {MAX_OPENCODE_VARIANT_LEN})"
            )));
        }
    } else if !effort_levels(kind).contains(&effort.as_str()) {
        return Err(ForjaError::InvalidInput(format!(
            "esfuerzo \"{}\" no válido para {kind} (usa {})",
            raw.trim(),
            effort_levels_es(kind)
        )));
    }
    Ok(effort)
}

/// Prioridad: tarea > ajustes > ninguno (si no se configura no se pasa ningún flag).
/// No valida: eso lo hace [`validate_effort`] en cada punto de entrada.
pub fn resolve_effort(
    kind: AgentKind,
    task_override: Option<&str>,
    settings: &BTreeMap<String, String>,
) -> Option<String> {
    if !EFFORT_AGENTS.contains(&kind) {
        return None;
    }
    let nonblank = |s: &str| (!s.trim().is_empty()).then(|| normalize_effort_alias(s));
    task_override.and_then(nonblank).or_else(|| {
        settings
            .get(&kind.to_string())
            .map(String::as_str)
            .and_then(nonblank)
    })
}

/// Modelo de opencode con la variante de razonamiento: `provider/model#variante`.
/// Un esfuerzo explícito reemplaza la variante que ya traiga el modelo.
pub fn opencode_model_with_effort(model: &str, effort: Option<&str>) -> String {
    match effort {
        Some(e) if !e.is_empty() => {
            let base = model.split('#').next().unwrap_or(model);
            format!("{base}#{e}")
        }
        _ => model.to_string(),
    }
}

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
    if !MODEL_AGENTS.contains(&kind) {
        return None;
    }
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
        (AgentKind::Hermes, m) => Arc::new(HermesAgent {
            model: m,
            ..HermesAgent::default()
        }),
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
        let h = map(&[("hermes", "gpt-x")]);
        assert_eq!(
            build_agent(AgentKind::Hermes, None, &h).model().as_deref(),
            Some("gpt-x")
        );
    }

    #[test]
    fn effort_validation_per_family() {
        for ok in ["low", " HIGH ", "max", "xhigh", "medium"] {
            assert!(validate_effort(AgentKind::Claude, ok).is_ok(), "{ok}");
            assert!(validate_effort(AgentKind::Agy, ok).is_ok(), "{ok}");
        }
        assert_eq!(
            validate_effort(AgentKind::Claude, " HIGH ").unwrap(),
            "high"
        );
        assert!(validate_effort(AgentKind::Claude, "ultra").is_err());
        assert!(validate_effort(AgentKind::Agy, "none").is_err());
        for ok in ["none", "minimal", "ultra", "max"] {
            assert!(validate_effort(AgentKind::Hermes, ok).is_ok(), "{ok}");
        }
        assert!(validate_effort(AgentKind::Hermes, "extreme").is_err());
        assert!(validate_effort(AgentKind::Opencode, "high").is_ok());
        assert!(validate_effort(AgentKind::Opencode, "my_variant-2").is_ok());
        assert!(validate_effort(AgentKind::Opencode, "a#b").is_err());
        assert!(validate_effort(AgentKind::Opencode, "--x y").is_err());
        assert!(validate_effort(AgentKind::Opencode, &"a".repeat(21)).is_err());
        assert!(validate_effort(AgentKind::Opencode, &"a".repeat(20)).is_ok());
        assert!(validate_effort(AgentKind::Claude, "").is_err());
        let e = validate_effort(AgentKind::Local, "high")
            .unwrap_err()
            .to_string();
        assert!(e.contains("local no admite esfuerzo"), "{e}");
        assert!(validate_effort(AgentKind::Mock, "high").is_err());
        assert!(effort_levels(AgentKind::Local).is_empty());
        assert!(effort_levels(AgentKind::Mock).is_empty());
        assert_eq!(effort_levels(AgentKind::Hermes).len(), 8);
    }

    #[test]
    fn spanish_effort_aliases_normalize_for_every_family() {
        for (raw, canon) in [
            ("alto", "high"),
            (" Alto ", "high"),
            ("bajo", "low"),
            ("Med", "medium"),
            ("medio", "medium"),
            ("Muy alto", "xhigh"),
            ("muy_alto", "xhigh"),
            ("Máx", "max"),
            ("máximo", "max"),
            ("maximo", "max"),
        ] {
            for k in [AgentKind::Claude, AgentKind::Agy, AgentKind::Hermes] {
                assert_eq!(validate_effort(k, raw).unwrap(), canon, "{raw} {k}");
            }
            assert_eq!(validate_effort(AgentKind::Opencode, raw).unwrap(), canon);
        }
        assert_eq!(
            validate_effort(AgentKind::Hermes, "Mínimo").unwrap(),
            "minimal"
        );
        assert_eq!(
            validate_effort(AgentKind::Hermes, "minimo").unwrap(),
            "minimal"
        );
        assert_eq!(
            validate_effort(AgentKind::Hermes, "Ninguno").unwrap(),
            "none"
        );
        // No válido para la familia: error con la lista en español.
        let e = validate_effort(AgentKind::Claude, "ninguno")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("low (bajo)") && e.contains("xhigh (muy alto)"),
            "{e}"
        );
        assert!(validate_effort(AgentKind::Agy, "mínimo").is_err());
        assert_eq!(
            resolve_effort(AgentKind::Claude, Some("alto"), &map(&[])).as_deref(),
            Some("high")
        );
        assert_eq!(
            resolve_effort(AgentKind::Claude, None, &map(&[("claude", "Muy alto")])).as_deref(),
            Some("xhigh")
        );
    }

    #[test]
    fn effort_priority_task_over_settings_over_none() {
        let s = map(&[("claude", "low"), ("local", "high")]);
        assert_eq!(
            resolve_effort(AgentKind::Claude, Some("MAX"), &s).as_deref(),
            Some("max")
        );
        assert_eq!(
            resolve_effort(AgentKind::Claude, None, &s).as_deref(),
            Some("low")
        );
        assert_eq!(resolve_effort(AgentKind::Agy, None, &s), None);
        assert_eq!(
            resolve_effort(AgentKind::Claude, Some(" "), &map(&[])),
            None
        );
        assert_eq!(resolve_effort(AgentKind::Local, Some("high"), &s), None);
    }

    #[test]
    fn opencode_variant_is_appended_and_replaced() {
        assert_eq!(opencode_model_with_effort("p/m", None), "p/m");
        assert_eq!(opencode_model_with_effort("p/m", Some("high")), "p/m#high");
        assert_eq!(
            opencode_model_with_effort("p/m#low", Some("high")),
            "p/m#high"
        );
        assert_eq!(opencode_model_with_effort("p/m#low", None), "p/m#low");
    }

    #[test]
    fn with_model_keeps_kind_and_changes_model() {
        let base = ClaudeAgent::default();
        let swapped = Agent::with_model(&base, "haiku").expect("claude admite modelo");
        assert_eq!(swapped.kind(), AgentKind::Claude);
        assert_eq!(swapped.model().as_deref(), Some("haiku"));
        assert!(MockAgent::default().with_model("x").is_none());
        let hermes = Agent::with_model(&HermesAgent::default(), "x").expect("hermes admite -m");
        assert_eq!(hermes.model().as_deref(), Some("x"));
    }
}
