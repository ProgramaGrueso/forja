use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

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
        Ok(())
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

pub fn save_settings(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("no se pudo crear {}: {err}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(settings)
        .map_err(|err| format!("no se pudo serializar ajustes: {err}"))?;
    std::fs::write(path, json)
        .map_err(|err| format!("no se pudo escribir {}: {err}", path.display()))
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
}
