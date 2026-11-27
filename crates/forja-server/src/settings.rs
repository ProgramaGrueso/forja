use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use forja_core::{default_sensitive_agents, AgentKind};
use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_sensitive_agents<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AgentKind>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Vec::<String>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let normalized = item.trim().to_ascii_lowercase();
        let kind = normalized.parse::<AgentKind>().map_err(|_| {
            serde::de::Error::custom(format!(
                "sensitive_agents: agente desconocido \"{item}\" (usa claude, agy, opencode, hermes, local o mock)"
            ))
        })?;
        out.push(kind);
    }
    Ok(out)
}

/// Ajustes editables desde el panel. Los cambios de concurrencia, store y puerto
/// se persisten y se aplican en el siguiente arranque de `forja serve`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub max_concurrency: usize,
    pub store_dir: String,
    pub port: u16,
    pub quota_path: String,
    #[serde(default)]
    pub hide_mascot: bool,
    /// Modelo por agente (claude, agy, opencode, local). Sin entrada => default en código.
    #[serde(default)]
    pub agent_models: BTreeMap<String, String>,
    /// Esfuerzo de razonamiento por agente (claude, agy, opencode, hermes). Sin entrada
    /// => no se pasa ningún flag. `local` no lo admite.
    #[serde(default)]
    pub agent_effort: BTreeMap<String, String>,
    /// Perfil de Hermes del orquestador. None => env FORJA_ORCHESTRATOR_PROFILE o `default`.
    #[serde(default)]
    pub orchestrator_profile: Option<String>,
    /// Agente que hace de orquestador: hermes | claude | opencode | agy | local.
    /// None => env FORJA_ORCHESTRATOR_AGENT o el primero disponible.
    #[serde(default)]
    pub orchestrator_agent: Option<String>,
    /// Perfiles de Hermes que nunca se listan ni se pueden elegir (se suman a
    /// `FORJA_HIDDEN_PROFILES`). Vacío por defecto.
    #[serde(default)]
    pub hidden_profiles: Vec<String>,
    /// Agentes permitidos para tareas con privacidad `sensitive`. Por defecto solo `claude`.
    #[serde(
        default = "default_sensitive_agents",
        deserialize_with = "deserialize_sensitive_agents"
    )]
    pub sensitive_agents: Vec<AgentKind>,
}

pub const MAX_CONCURRENCY_LIMIT: usize = 16;

const FORBIDDEN_PATH_SEGMENTS: [&str; 4] = [".hermes", ".ssh", ".gnupg", ".env"];

fn validate_safe_path(field: &str, raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err(format!("{field} no puede estar vacío"));
    }
    if raw.contains('\0') {
        return Err(format!("{field} contiene caracteres nulos inválidos"));
    }
    for comp in Path::new(raw).components() {
        if matches!(comp, Component::ParentDir) {
            return Err(format!("{field} no puede contener '..'"));
        }
        if let Component::Normal(seg) = comp {
            let s = seg.to_string_lossy();
            if FORBIDDEN_PATH_SEGMENTS
                .iter()
                .any(|f| s.eq_ignore_ascii_case(f))
            {
                return Err(format!("{field} apunta a una ruta protegida ({s})"));
            }
        }
    }
    Ok(())
}

impl Settings {
    /// Ajustes por defecto para un store dado (los que usa `forja init` cuando
    /// todavía no existe `settings.json`).
    pub fn with_defaults(store_dir: &str) -> Self {
        Self {
            max_concurrency: 2,
            store_dir: store_dir.to_string(),
            port: 7777,
            quota_path: crate::quota::default_manual_path()
                .to_string_lossy()
                .into_owned(),
            hide_mascot: false,
            agent_models: BTreeMap::new(),
            agent_effort: BTreeMap::new(),
            orchestrator_profile: None,
            orchestrator_agent: None,
            hidden_profiles: Vec::new(),
            sensitive_agents: default_sensitive_agents(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_concurrency == 0 || self.max_concurrency > MAX_CONCURRENCY_LIMIT {
            return Err(format!(
                "max_concurrency debe estar entre 1 y {MAX_CONCURRENCY_LIMIT}"
            ));
        }
        if self.port < 1024 {
            return Err("port debe estar entre 1024 y 65535".to_string());
        }
        validate_safe_path("store_dir", &self.store_dir)?;
        validate_safe_path("quota_path", &self.quota_path)?;
        if !self.quota_path.ends_with(".json") {
            return Err("quota_path debe apuntar a un archivo .json".to_string());
        }
        for (agent, model) in &self.agent_models {
            let kind = agent
                .parse::<forja_core::AgentKind>()
                .ok()
                .filter(|k| forja_agent::MODEL_AGENTS.contains(k))
                .ok_or_else(|| format!("agent_models: el agente \"{agent}\" no admite modelo"))?;
            forja_agent::validate_model(model).map_err(|e| format!("agent_models[{kind}]: {e}"))?;
        }
        for (agent, effort) in &self.agent_effort {
            let kind = agent
                .parse::<forja_core::AgentKind>()
                .map_err(|_| format!("agent_effort: agente desconocido \"{agent}\""))?;
            forja_agent::validate_effort(kind, effort)
                .map_err(|e| format!("agent_effort[{kind}]: {e}"))?;
        }
        // Aquí solo se valida el formato (función pura); que el perfil exista y no
        // esté oculto lo comprueba el orquestador con las reglas de perfiles.
        if let Some(profile) = &self.orchestrator_profile {
            let normalized = forja_agent::hermes::validate_profile_format(profile)
                .map_err(|e| format!("orchestrator_profile: {e}"))?;
            if self.hidden_profiles.iter().any(|h| h == &normalized) {
                return Err(format!(
                    "orchestrator_profile: \"{normalized}\" está en hidden_profiles"
                ));
            }
        }
        if let Some(agent) = &self.orchestrator_agent {
            if !crate::orchestrator::ORCHESTRATOR_AGENTS.contains(&agent.as_str()) {
                return Err(format!(
                    "orchestrator_agent: \"{agent}\" no es válido (usa {})",
                    crate::orchestrator::ORCHESTRATOR_AGENTS.join(", ")
                ));
            }
        }
        for hidden in &self.hidden_profiles {
            forja_agent::hermes::validate_profile_format(hidden)
                .map_err(|e| format!("hidden_profiles: {e}"))?;
        }
        if self.sensitive_agents.is_empty() {
            return Err(
                "sensitive_agents no puede estar vacío (elige al menos un agente)".to_string(),
            );
        }
        let mut seen = Vec::with_capacity(self.sensitive_agents.len());
        for &agent in &self.sensitive_agents {
            if seen.contains(&agent) {
                return Err(format!(
                    "sensitive_agents contiene el agente duplicado \"{agent}\""
                ));
            }
            seen.push(agent);
        }
        Ok(())
    }

    /// Normaliza antes de validar: recorta modelos, quita entradas vacías y
    /// convierte un perfil en blanco en `None` (vuelve al valor por defecto).
    pub fn normalize(&mut self) {
        self.agent_models = std::mem::take(&mut self.agent_models)
            .into_iter()
            .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        self.agent_effort = std::mem::take(&mut self.agent_effort)
            .into_iter()
            .map(|(k, v)| {
                (
                    k.trim().to_lowercase(),
                    forja_agent::normalize_effort_alias(&v),
                )
            })
            .filter(|(_, v)| !v.is_empty())
            .collect();
        self.orchestrator_profile = self
            .orchestrator_profile
            .take()
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty());
        self.orchestrator_agent = self
            .orchestrator_agent
            .take()
            .map(|a| a.trim().to_lowercase())
            .filter(|a| !a.is_empty());
        let mut hidden: Vec<String> = Vec::new();
        for h in std::mem::take(&mut self.hidden_profiles) {
            let h = h.trim().to_lowercase();
            if !h.is_empty() && !hidden.contains(&h) {
                hidden.push(h);
            }
        }
        self.hidden_profiles = hidden;
    }

    /// Campos cuyo cambio exige reiniciar `forja serve` para tener efecto.
    pub fn restart_required(&self, previous: &Settings) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.store_dir != previous.store_dir {
            fields.push("store_dir");
        }
        if self.port != previous.port {
            fields.push("port");
        }
        fields
    }
}

/// Lee ajustes persistidos. Archivo inexistente => `Ok(None)`.
pub fn load_settings(path: &Path) -> Result<Option<Settings>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("no se pudo leer {}: {err}", path.display()))?;
    let settings: Settings = serde_json::from_str(&raw)
        .map_err(|err| format!("ajustes inválidos en {}: {err}", path.display()))?;
    settings.validate()?;
    Ok(Some(settings))
}

/// Como [`load_settings`], pero un archivo ilegible o inválido no pasa en
/// silencio: guarda una copia y devuelve el aviso (se usan los defaults).
pub fn load_settings_or_warn(path: &Path) -> (Option<Settings>, Option<String>) {
    match load_settings(path) {
        Ok(settings) => (settings, None),
        Err(err) => {
            let copy = match backup_corrupt(path) {
                Ok(copy) => format!("copia del original en {}", copy.display()),
                Err(why) => format!("no se pudo copiar el original: {why}"),
            };
            let warning = format!(
                "no se pudieron cargar los ajustes de settings.json ({err}). Se usan los \
                 valores por defecto (revisa perfiles ocultos y agentes sensibles); {copy}"
            );
            (None, Some(warning))
        }
    }
}

/// Copia un `settings.json` corrupto a `<nombre>.corrupt[.N]` sin pisar copias
/// distintas. Si ya hay una copia idéntica, la reutiliza.
fn backup_corrupt(path: &Path) -> Result<PathBuf, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let permissions = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
    let name = path
        .file_name()
        .ok_or_else(|| "ruta sin nombre de archivo".to_string())?
        .to_string_lossy()
        .into_owned();
    for n in 0..1000 {
        let suffix = if n == 0 {
            String::new()
        } else {
            format!(".{n}")
        };
        let copy = path.with_file_name(format!("{name}.corrupt{suffix}"));
        match fs::read(&copy) {
            Ok(existing) if existing == bytes => return Ok(copy),
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::copy(path, &copy).map_err(|e| e.to_string())?;
                fs::set_permissions(&copy, permissions).map_err(|e| e.to_string())?;
                return Ok(copy);
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("demasiadas copias .corrupt".to_string())
}

/// Guarda los ajustes. Si lo que había era un archivo corrupto, antes lo copia
/// y devuelve la ruta de la copia.
pub fn save_settings(path: &Path, settings: &Settings) -> Result<Option<PathBuf>, String> {
    let backup =
        if path.is_file() && load_settings(path).is_err() {
            Some(backup_corrupt(path).map_err(|e| {
                format!("no se sobrescribe un settings.json corrupto sin copia: {e}")
            })?)
        } else {
            None
        };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("no se pudo crear {}: {err}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(settings)
        .map_err(|err| format!("no se pudo serializar ajustes: {err}"))?;
    std::fs::write(path, json)
        .map_err(|err| format!("no se pudo escribir {}: {err}", path.display()))?;
    Ok(backup)
}

/// Cambios que `forja init` aplica sobre `settings.json` sin tocar el resto.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsPatch {
    pub orchestrator_agent: Option<String>,
    pub orchestrator_profile: Option<String>,
    /// Se fusiona clave a clave con `agent_models` existente.
    pub agent_models: BTreeMap<String, String>,
    /// Se fusiona clave a clave con `agent_effort` existente.
    pub agent_effort: BTreeMap<String, String>,
    /// Se UNE con `hidden_profiles` existente (nunca se quita protección).
    pub hidden_profiles: Vec<String>,
}

/// Aplica `patch` sobre el archivo de ajustes y lo guarda. Conserva todos los
/// campos existentes (también los desconocidos); si el archivo no existe parte
/// de `defaults`. Devuelve los ajustes resultantes, ya validados.
pub fn apply_patch(
    path: &Path,
    patch: &SettingsPatch,
    defaults: &Settings,
) -> Result<Settings, String> {
    let defaults_value = serde_json::to_value(defaults)
        .map_err(|err| format!("no se pudo serializar ajustes: {err}"))?;
    let mut root = if path.exists() {
        let raw = std::fs::read_to_string(path)
            .map_err(|err| format!("no se pudo leer {}: {err}", path.display()))?;
        serde_json::from_str::<serde_json::Value>(&raw)
            .map_err(|err| format!("ajustes inválidos en {}: {err}", path.display()))?
    } else {
        defaults_value.clone()
    };
    let obj = root.as_object_mut().ok_or_else(|| {
        format!(
            "ajustes inválidos en {}: se esperaba un objeto",
            path.display()
        )
    })?;
    // Campos obligatorios ausentes en un archivo viejo o parcial.
    if let Some(d) = defaults_value.as_object() {
        for key in ["max_concurrency", "store_dir", "port", "quota_path"] {
            if !obj.contains_key(key) {
                if let Some(v) = d.get(key) {
                    obj.insert(key.to_string(), v.clone());
                }
            }
        }
    }

    let set_opt = |obj: &mut serde_json::Map<String, serde_json::Value>,
                   key: &str,
                   value: &Option<String>| {
        if let Some(v) = value {
            obj.insert(key.to_string(), serde_json::Value::String(v.clone()));
        }
    };
    set_opt(obj, "orchestrator_agent", &patch.orchestrator_agent);
    set_opt(obj, "orchestrator_profile", &patch.orchestrator_profile);

    if !patch.agent_models.is_empty() {
        let entry = obj
            .entry("agent_models".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(models) = entry.as_object_mut() {
            for (agent, model) in &patch.agent_models {
                models.insert(agent.clone(), serde_json::Value::String(model.clone()));
            }
        }
    }

    if !patch.agent_effort.is_empty() {
        let entry = obj
            .entry("agent_effort".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(efforts) = entry.as_object_mut() {
            for (agent, effort) in &patch.agent_effort {
                efforts.insert(agent.clone(), serde_json::Value::String(effort.clone()));
            }
        }
    }

    if !patch.hidden_profiles.is_empty() {
        let mut hidden: Vec<String> = obj
            .get("hidden_profiles")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for h in &patch.hidden_profiles {
            if !hidden.contains(h) {
                hidden.push(h.clone());
            }
        }
        obj.insert("hidden_profiles".to_string(), serde_json::json!(hidden));
    }

    let mut settings: Settings = serde_json::from_value(root.clone())
        .map_err(|err| format!("ajustes inválidos en {}: {err}", path.display()))?;
    settings.normalize();
    settings.validate()?;

    // Se escribe el JSON fusionado (conserva campos desconocidos), pero con los
    // valores normalizados de los campos que Forja entiende.
    if let (Some(obj), Ok(norm)) = (root.as_object_mut(), serde_json::to_value(&settings)) {
        if let Some(norm) = norm.as_object() {
            for (k, v) in norm {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("no se pudo crear {}: {err}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&root)
        .map_err(|err| format!("no se pudo serializar ajustes: {err}"))?;
    std::fs::write(path, json)
        .map_err(|err| format!("no se pudo escribir {}: {err}", path.display()))?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn valid() -> Settings {
        Settings {
            max_concurrency: 2,
            store_dir: "/tmp/store".to_string(),
            port: 7777,
            quota_path: "/tmp/quota.json".to_string(),
            hide_mascot: false,
            agent_models: BTreeMap::new(),
            agent_effort: BTreeMap::new(),
            orchestrator_profile: None,
            orchestrator_agent: None,
            hidden_profiles: Vec::new(),
            sensitive_agents: default_sensitive_agents(),
        }
    }

    #[test]
    fn settings_validate_accepts_and_rejects() {
        assert!(valid().validate().is_ok());

        let mut s = valid();
        s.max_concurrency = 0;
        assert!(s.validate().is_err());
        s.max_concurrency = 17;
        assert!(s.validate().is_err());

        let mut s = valid();
        s.port = 80;
        assert!(s.validate().is_err());

        let mut s = valid();
        s.store_dir = "  ".to_string();
        assert!(s.validate().is_err());

        let mut s = valid();
        s.quota_path = "/tmp/quota.txt".to_string();
        assert!(s.validate().is_err());

        // Pruebas adversarias: path traversal y rutas protegidas
        let mut s = valid();
        s.quota_path = "/tmp/../etc/quota.json".to_string();
        assert!(s.validate().is_err());

        let mut s = valid();
        s.quota_path = "/home/user/.hermes/config.json".to_string();
        assert!(s.validate().is_err());

        let mut s = valid();
        s.store_dir = "/home/user/.ssh/store".to_string();
        assert!(s.validate().is_err());

        let mut s = valid();
        s.quota_path = "/tmp/bad\0.json".to_string();
        assert!(s.validate().is_err());
    }

    #[test]
    fn settings_validate_agent_models_and_profile() {
        let mut s = valid();
        s.agent_models.insert("claude".into(), "opus".into());
        s.orchestrator_profile = Some("claudia".into());
        assert!(s.validate().is_ok());

        for bad in ["", "   ", "a\nb", "a\tb", "--flag", &"x".repeat(101)] {
            let mut s = valid();
            s.agent_models.insert("claude".into(), bad.to_string());
            assert!(s.validate().is_err(), "modelo {bad:?} debía rechazarse");
        }
        let mut s = valid();
        s.agent_models.insert("mock".into(), "x".into());
        assert!(s.validate().is_err());
        s.agent_models.clear();
        s.agent_models.insert("nope".into(), "x".into());
        assert!(s.validate().is_err());

        // Formato inválido o perfil oculto: nunca puede elegirse.
        for bad in ["../default", "root x", "", "-x"] {
            let mut s = valid();
            s.orchestrator_profile = Some(bad.to_string());
            assert!(s.validate().is_err(), "perfil {bad:?} debía rechazarse");
        }
        let mut s = valid();
        s.orchestrator_profile = Some("privado".into());
        s.hidden_profiles = vec!["privado".into()];
        assert!(s.validate().is_err());
        s.hidden_profiles = vec!["../x".into()];
        s.orchestrator_profile = None;
        assert!(s.validate().is_err());
    }

    #[test]
    fn settings_validate_orchestrator_agent() {
        for ok in ["hermes", "claude", "opencode", "agy", "local"] {
            let mut s = valid();
            s.orchestrator_agent = Some(ok.into());
            assert!(s.validate().is_ok(), "{ok}");
        }
        for bad in ["mock", "gpt", "", "Claude"] {
            let mut s = valid();
            s.orchestrator_agent = Some(bad.into());
            assert!(s.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn agent_effort_validates_per_family_and_normalizes() {
        let mut s = valid();
        s.agent_effort.insert(" Claude ".into(), " HIGH ".into());
        s.agent_effort.insert("agy".into(), "  ".into());
        s.normalize();
        assert_eq!(s.agent_effort.len(), 1);
        assert_eq!(s.agent_effort["claude"], "high");
        assert!(s.validate().is_ok());
        for (agent, bad) in [
            ("claude", "ultra"),
            ("hermes", "extreme"),
            ("opencode", "a b"),
            ("local", "high"),
            ("mock", "low"),
            ("nope", "low"),
        ] {
            let mut s = valid();
            s.agent_effort.insert(agent.into(), bad.into());
            assert!(s.validate().is_err(), "{agent}={bad}");
        }
        let mut s = valid();
        s.agent_effort.insert("hermes".into(), "ultra".into());
        s.agent_effort.insert("opencode".into(), "high".into());
        assert!(s.validate().is_ok());
        let e = {
            let mut s = valid();
            s.agent_effort.insert("local".into(), "high".into());
            s.validate().unwrap_err()
        };
        assert!(e.contains("no admite esfuerzo"), "{e}");
        // Ajustes viejos sin el campo siguen cargando.
        let old =
            r#"{"max_concurrency":2,"store_dir":"/tmp/s","port":7777,"quota_path":"/tmp/q.json"}"#;
        let parsed: Settings = serde_json::from_str(old).unwrap();
        assert!(parsed.agent_effort.is_empty());
    }

    #[test]
    fn settings_normalize_trims_and_clears_blanks() {
        let mut s = valid();
        s.agent_models.insert(" Claude ".into(), " opus ".into());
        s.agent_models.insert("agy".into(), "  ".into());
        s.orchestrator_profile = Some("  ".into());
        s.normalize();
        assert_eq!(s.agent_models.len(), 1);
        assert_eq!(s.agent_models["claude"], "opus");
        assert_eq!(s.orchestrator_profile, None);
        s.orchestrator_profile = Some(" Otro ".into());
        s.orchestrator_agent = Some(" Claude ".into());
        s.hidden_profiles = vec![" Priv ".into(), "priv".into(), "  ".into(), "b".into()];
        s.normalize();
        assert_eq!(s.orchestrator_profile.as_deref(), Some("otro"));
        assert_eq!(s.orchestrator_agent.as_deref(), Some("claude"));
        assert_eq!(s.hidden_profiles, vec!["priv", "b"]);
        assert!(s.validate().is_ok());
        s.orchestrator_agent = Some("  ".into());
        s.normalize();
        assert_eq!(s.orchestrator_agent, None);
    }

    #[test]
    fn settings_old_json_without_new_fields_still_loads() {
        let old =
            r#"{"max_concurrency":2,"store_dir":"/tmp/s","port":7777,"quota_path":"/tmp/q.json"}"#;
        let s: Settings = serde_json::from_str(old).expect("compat");
        assert!(s.agent_models.is_empty());
        assert_eq!(s.orchestrator_profile, None);
        assert_eq!(s.orchestrator_agent, None);
        assert!(s.hidden_profiles.is_empty());
    }

    #[test]
    fn patch_creates_file_from_defaults() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("store/settings.json");
        let defaults = Settings::with_defaults("/tmp/s");
        let patch = SettingsPatch {
            agent_effort: [("claude".to_string(), "high".to_string())].into(),
            orchestrator_agent: Some("claude".into()),
            agent_models: [("claude".to_string(), "opus".to_string())].into(),
            hidden_profiles: vec!["priv".into()],
            ..Default::default()
        };
        let out = apply_patch(&path, &patch, &defaults).expect("crear");
        assert_eq!(out.orchestrator_agent.as_deref(), Some("claude"));
        assert_eq!(out.port, 7777);
        let back = load_settings(&path).expect("leer").expect("existe");
        assert_eq!(back, out);
        assert_eq!(back.hidden_profiles, vec!["priv"]);
    }

    #[test]
    fn patch_merges_without_clobbering_existing_fields() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"max_concurrency":4,"store_dir":"/tmp/x","port":9000,"quota_path":"/tmp/q.json",
                "hide_mascot":true,"agent_models":{"agy":"g-1","claude":"haiku"},
                "hidden_profiles":["a"],"campo_futuro":{"x":1}}"#,
        )
        .expect("escribir");
        let patch = SettingsPatch {
            orchestrator_agent: Some("opencode".into()),
            orchestrator_profile: None,
            agent_models: [("claude".to_string(), "opus".to_string())].into(),
            agent_effort: Default::default(),
            hidden_profiles: vec!["b".into(), "a".into()],
        };
        let out = apply_patch(&path, &patch, &Settings::with_defaults("/otro")).expect("merge");
        assert_eq!(out.max_concurrency, 4);
        assert_eq!(out.port, 9000);
        assert_eq!(out.store_dir, "/tmp/x");
        assert!(out.hide_mascot);
        assert_eq!(out.agent_models["agy"], "g-1", "no pisa otros agentes");
        assert_eq!(out.agent_models["claude"], "opus");
        assert_eq!(out.hidden_profiles, vec!["a", "b"], "une, no reemplaza");
        assert_eq!(out.orchestrator_agent.as_deref(), Some("opencode"));
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("leer")).expect("json");
        assert_eq!(raw["campo_futuro"]["x"], 1, "conserva campos desconocidos");

        // Un parche vacío no cambia nada.
        let again = apply_patch(
            &path,
            &SettingsPatch::default(),
            &Settings::with_defaults("/o"),
        )
        .expect("vacío");
        assert_eq!(again, out);
    }

    #[test]
    fn patch_rejects_invalid_result_and_broken_file() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("settings.json");
        let defaults = Settings::with_defaults("/tmp/s");
        let bad = SettingsPatch {
            orchestrator_agent: Some("nope".into()),
            ..Default::default()
        };
        assert!(apply_patch(&path, &bad, &defaults).is_err());
        assert!(!path.exists(), "no escribe si es inválido");
        std::fs::write(&path, "{roto").expect("escribir");
        assert!(apply_patch(&path, &SettingsPatch::default(), &defaults).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("leer"), "{roto");
    }

    #[test]
    fn settings_restart_required_lists_changed_fields() {
        let prev = valid();
        let mut next = valid();
        next.hide_mascot = true;
        next.quota_path = "/tmp/otra.json".to_string();
        assert!(next.restart_required(&prev).is_empty());

        next.port = 7788;
        next.max_concurrency = 4;
        assert_eq!(next.restart_required(&prev), vec!["port"]);
    }

    #[test]
    fn settings_roundtrip_file() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sub/settings.json");
        assert_eq!(load_settings(&path).expect("inexistente"), None);
        save_settings(&path, &valid()).expect("guardar");
        assert_eq!(load_settings(&path).expect("leer"), Some(valid()));

        std::fs::write(&path, "{roto").expect("escribir roto");
        assert!(load_settings(&path).is_err());
    }

    #[test]
    fn sensitive_agents_defaults_normalizes_and_validates() {
        // Default estricto: solo claude (también al cargar JSON viejo sin el campo).
        let defaults = Settings::with_defaults("/tmp/store");
        assert_eq!(defaults.sensitive_agents, vec![AgentKind::Claude]);
        let old =
            r#"{"max_concurrency":2,"store_dir":"/tmp/s","port":7777,"quota_path":"/tmp/q.json"}"#;
        let parsed: Settings = serde_json::from_str(old).expect("compat");
        assert_eq!(parsed.sensitive_agents, vec![AgentKind::Claude]);
        assert!(parsed.validate().is_ok());

        // Normalización a minúsculas y recorte al deserializar.
        let upper = r#"{"max_concurrency":2,"store_dir":"/tmp/s","port":7777,"quota_path":"/tmp/q.json","sensitive_agents":[" LOCAL ","Claude"]}"#;
        let parsed: Settings = serde_json::from_str(upper).expect("normalizar");
        assert_eq!(
            parsed.sensitive_agents,
            vec![AgentKind::Local, AgentKind::Claude]
        );
        assert!(parsed.validate().is_ok());

        // Lista vacía se rechaza.
        let mut s = valid();
        s.sensitive_agents = vec![];
        assert!(s.validate().is_err());

        // Duplicados se rechazan (tanto en struct como por mayúsculas en JSON).
        let mut s = valid();
        s.sensitive_agents = vec![AgentKind::Claude, AgentKind::Claude];
        assert!(s.validate().is_err());
        let dup_json = r#"{"max_concurrency":2,"store_dir":"/tmp/s","port":7777,"quota_path":"/tmp/q.json","sensitive_agents":["claude","CLAUDE"]}"#;
        let dup: Settings = serde_json::from_str(dup_json).expect("deserializa antes de validar");
        assert!(dup.validate().is_err());

        // Agente desconocido se rechaza al deserializar.
        let unknown_json = r#"{"max_concurrency":2,"store_dir":"/tmp/s","port":7777,"quota_path":"/tmp/q.json","sensitive_agents":["gpt"]}"#;
        assert!(serde_json::from_str::<Settings>(unknown_json).is_err());
    }
}
