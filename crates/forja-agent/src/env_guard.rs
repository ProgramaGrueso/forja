//! Variables de entorno que no deben heredar los procesos de agente (el token
//! de autenticación de la API de Forja).
//!
//! Es una defensa parcial: un agente corre con el mismo usuario que Forja y
//! puede leer la línea de órdenes o los archivos de ese usuario.

use std::ffi::{OsStr, OsString};
use std::process::Command;
use std::sync::RwLock;

/// Nombres que se quitan siempre.
const TOKEN_VARS: &[&str] = &["FORJA_AUTH_TOKEN", "FORJA_TOKEN"];
/// A partir de esta longitud se quita también la variable que *contenga* el
/// secreto; por debajo solo la que sea igual (evita falsos positivos).
const MIN_SUBSTRING_LEN: usize = 8;

static SECRETS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Registra un secreto: ninguna variable heredada que lo lleve llegará a un agente.
pub fn hide_secret(secret: &str) {
    let secret = secret.trim();
    if secret.is_empty() {
        return;
    }
    let mut secrets = SECRETS.write().unwrap_or_else(|e| e.into_inner());
    if !secrets.iter().any(|s| s == secret) {
        secrets.push(secret.to_string());
    }
}

fn blocked_vars(
    vars: impl Iterator<Item = (OsString, OsString)>,
    secrets: &[String],
) -> Vec<OsString> {
    let carries = |value: &OsStr| {
        let value = value.to_string_lossy();
        secrets.iter().any(|s| {
            value == s.as_str() || (s.len() >= MIN_SUBSTRING_LEN && value.contains(s.as_str()))
        })
    };
    vars.filter(|(name, value)| TOKEN_VARS.iter().any(|t| name == t) || carries(value))
        .map(|(name, _)| name)
        .collect()
}

/// Quita del hijo las variables heredadas que llevan el token.
pub fn scrub_env(command: &mut Command) {
    let secrets = SECRETS.read().unwrap_or_else(|e| e.into_inner()).clone();
    for name in blocked_vars(std::env::vars_os(), &secrets) {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Runner;

    #[test]
    fn blocks_token_names_and_values() {
        let vars = [
            ("FORJA_AUTH_TOKEN", "x"),
            ("OTRA", "Bearer secreto-largo-123"),
            ("CORTA", "abc"),
            ("PATH", "/usr/abc/bin"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let secrets = ["secreto-largo-123".to_string(), "abc".to_string()];
        assert_eq!(
            blocked_vars(vars.into_iter(), &secrets),
            ["FORJA_AUTH_TOKEN", "OTRA", "CORTA"].map(OsString::from)
        );
    }

    #[tokio::test]
    async fn agent_processes_do_not_inherit_a_hidden_secret() {
        // Cargo define esta variable al ejecutar las pruebas.
        let secret = std::env::var("CARGO_PKG_VERSION").expect("version");
        let argv = ["env".to_string()];
        let dir = std::env::temp_dir();
        let before = Runner::run_argv(&argv, &dir, 10).await.expect("env");
        assert!(before.stdout.contains("CARGO_PKG_VERSION="));
        hide_secret(&secret);
        let after = Runner::run_argv(&argv, &dir, 10).await.expect("env");
        assert!(!after.stdout.contains("CARGO_PKG_VERSION="));
        assert!(after.stdout.contains("PATH="));
    }
}
