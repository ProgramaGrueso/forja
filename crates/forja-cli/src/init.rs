//! Asistente de primer arranque (`forja init`, `forja doctor --fix`).
//!
//! Detecta los agentes instalados, pregunta (con un valor por defecto y Enter para
//! aceptarlo) qué agente orquesta, qué modelo usar por agente y qué perfiles de
//! Hermes ocultar, y escribe `<store_dir>/settings.json` fusionando con lo que ya
//! hubiera (sin pisar campos existentes). La entrada y la salida son inyectables
//! para poder probarlo con entrada simulada.

use std::io::{BufRead, Write};
use std::path::Path;

use forja_agent::hermes::{parse_hidden_list, validate_profile_format, ProfileRules};
use forja_core::{ForjaError, Result};
use forja_server::orchestrator::{agent_installed, AUTO_ORDER};
use forja_server::settings::{apply_patch, load_settings, Settings, SettingsPatch};

/// Lo que el asistente necesita saber del entorno (inyectable en tests).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitEnv {
    /// Agentes instalados, en el orden automático (claude, opencode, agy, local, hermes).
    pub installed: Vec<String>,
    /// Perfiles de Hermes descubiertos (incluye `default`); vacío si no hay Hermes.
    pub hermes_profiles: Vec<String>,
}

impl InitEnv {
    /// Detecta el entorno real: `PATH` y el directorio de perfiles de Hermes.
    pub fn detect() -> Self {
        let installed: Vec<String> = AUTO_ORDER
            .iter()
            .filter(|a| agent_installed(a))
            .map(|a| a.to_string())
            .collect();
        let hermes_profiles = if installed.iter().any(|a| a == "hermes") {
            ProfileRules::new().discover_all()
        } else {
            Vec::new()
        };
        Self {
            installed,
            hermes_profiles,
        }
    }
}

fn io_err(err: std::io::Error) -> ForjaError {
    ForjaError::Io(err)
}

/// Pregunta con valor por defecto. Enter, EOF o `--yes` aceptan el valor por defecto.
fn ask<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    yes: bool,
    question: &str,
    default: &str,
) -> Result<String> {
    if yes {
        writeln!(out, "{question} [{default}]: {default}").map_err(io_err)?;
        return Ok(default.to_string());
    }
    write!(out, "{question} [{default}]: ").map_err(io_err)?;
    out.flush().map_err(io_err)?;
    let mut line = String::new();
    let n = input.read_line(&mut line).map_err(io_err)?;
    let answer = line.trim();
    if n == 0 || answer.is_empty() {
        if n == 0 {
            writeln!(out).map_err(io_err)?;
        }
        return Ok(default.to_string());
    }
    Ok(answer.to_string())
}

/// Comando para arrancar Forja con este store.
pub fn start_command(store_dir: &Path) -> String {
    if store_dir == Path::new(".forja/store") {
        "forja serve --open".to_string()
    } else {
        format!("forja --store-dir {} serve --open", store_dir.display())
    }
}

/// Ejecuta el asistente y guarda `<store_dir>/settings.json`.
pub fn run_init<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    store_dir: &Path,
    yes: bool,
    env: &InitEnv,
) -> Result<Settings> {
    let settings_path = store_dir.join("settings.json");
    let existing = load_settings(&settings_path).map_err(ForjaError::InvalidInput)?;
    let mut patch = SettingsPatch::default();

    writeln!(
        out,
        "Forja init: configuración inicial ({})",
        settings_path.display()
    )
    .map_err(io_err)?;
    writeln!(out, "Agentes detectados:").map_err(io_err)?;
    for agent in AUTO_ORDER {
        let ok = env.installed.iter().any(|a| a == agent);
        writeln!(out, "  [{}] {agent}", if ok { "ok" } else { "--" }).map_err(io_err)?;
    }
    if !yes {
        writeln!(out, "Enter acepta el valor entre corchetes.").map_err(io_err)?;
    }

    // 1. Agente del orquestador.
    let mut chosen_agent: Option<String> = None;
    if env.installed.is_empty() {
        writeln!(
            out,
            "No hay ningún agente instalado: el orquestador queda en automático \
             (instala claude, opencode, agy, local o hermes; con `--agent mock` puedes probar)."
        )
        .map_err(io_err)?;
    } else {
        let saved = existing
            .as_ref()
            .and_then(|s| s.orchestrator_agent.clone())
            .filter(|a| env.installed.contains(a));
        let default = saved.unwrap_or_else(|| env.installed[0].clone());
        let answer = ask(
            input,
            out,
            yes,
            &format!("Agente del orquestador ({})", env.installed.join("/")),
            &default,
        )?
        .to_lowercase();
        let agent = if env.installed.contains(&answer) {
            answer
        } else {
            writeln!(out, "  \"{answer}\" no está instalado: se usa {default}.").map_err(io_err)?;
            default
        };
        patch.orchestrator_agent = Some(agent.clone());
        chosen_agent = Some(agent);
    }

    // 2. Modelo por agente instalado.
    let saved_models = existing
        .as_ref()
        .map(|s| s.agent_models.clone())
        .unwrap_or_default();
    for kind in forja_agent::MODEL_AGENTS {
        let name = kind.to_string();
        if !env.installed.contains(&name) {
            continue;
        }
        let code_default = forja_agent::default_model(kind).unwrap_or_default();
        let default = saved_models
            .get(&name)
            .map(String::as_str)
            .unwrap_or(code_default)
            .to_string();
        let answer = ask(
            input,
            out,
            yes,
            &format!("Modelo para {name} (default del código: {code_default})"),
            &default,
        )?;
        let model = match forja_agent::validate_model(&answer) {
            Ok(m) => m,
            Err(e) => {
                writeln!(out, "  modelo no válido ({e}): se conserva {default}.")
                    .map_err(io_err)?;
                default.clone()
            }
        };
        // Solo se guarda si difiere del default del código o ya había una entrada.
        if model != code_default || saved_models.contains_key(&name) {
            patch.agent_models.insert(name, model);
        }
    }

    // 3. Hermes: perfiles a ocultar y, si orquesta, qué perfil.
    if env.installed.iter().any(|a| a == "hermes") {
        let others: Vec<&String> = env
            .hermes_profiles
            .iter()
            .filter(|p| p.as_str() != "default")
            .collect();
        let saved_hidden = existing
            .as_ref()
            .map(|s| s.hidden_profiles.clone())
            .unwrap_or_default();
        if !others.is_empty() {
            writeln!(
                out,
                "Perfiles de Hermes encontrados: {}",
                env.hermes_profiles.join(", ")
            )
            .map_err(io_err)?;
            let answer = ask(
                input,
                out,
                yes,
                "Perfiles a ocultar (separados por comas; vacío = ninguno)",
                &saved_hidden.join(","),
            )?;
            for h in parse_hidden_list(&answer) {
                if validate_profile_format(&h).is_err() {
                    writeln!(
                        out,
                        "  \"{h}\" no es un nombre de perfil válido: se ignora."
                    )
                    .map_err(io_err)?;
                    continue;
                }
                if !patch.hidden_profiles.contains(&h) {
                    patch.hidden_profiles.push(h);
                }
            }
            writeln!(
                out,
                "  (los perfiles ocultos nunca se listan ni se pueden elegir; para mostrarlos de nuevo edita hidden_profiles en settings.json)"
            )
            .map_err(io_err)?;
        }

        if chosen_agent.as_deref() == Some("hermes") {
            let hidden: Vec<String> = saved_hidden
                .iter()
                .chain(patch.hidden_profiles.iter())
                .cloned()
                .collect();
            let visible: Vec<String> = env
                .hermes_profiles
                .iter()
                .filter(|p| !hidden.contains(p))
                .cloned()
                .collect();
            if visible.len() > 1 {
                let saved = existing
                    .as_ref()
                    .and_then(|s| s.orchestrator_profile.clone())
                    .filter(|p| visible.contains(p))
                    .unwrap_or_else(|| "default".to_string());
                let answer = ask(
                    input,
                    out,
                    yes,
                    &format!("Perfil de Hermes del orquestador ({})", visible.join("/")),
                    &saved,
                )?
                .to_lowercase();
                let profile = if visible.contains(&answer) {
                    answer
                } else {
                    writeln!(out, "  \"{answer}\" no está disponible: se usa {saved}.")
                        .map_err(io_err)?;
                    saved
                };
                patch.orchestrator_profile = Some(profile);
            }
        }
    }

    let defaults = Settings::with_defaults(&store_dir.to_string_lossy());
    let settings =
        apply_patch(&settings_path, &patch, &defaults).map_err(ForjaError::InvalidInput)?;

    writeln!(out, "Ajustes guardados en {}", settings_path.display()).map_err(io_err)?;
    if let Some(agent) = &settings.orchestrator_agent {
        writeln!(out, "  orquestador: {agent}").map_err(io_err)?;
    }
    for (agent, model) in &settings.agent_models {
        writeln!(out, "  modelo de {agent}: {model}").map_err(io_err)?;
    }
    if !settings.hidden_profiles.is_empty() {
        writeln!(
            out,
            "  perfiles ocultos: {}",
            settings.hidden_profiles.join(", ")
        )
        .map_err(io_err)?;
    }
    writeln!(out, "Para arrancar: {}", start_command(store_dir)).map_err(io_err)?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::*;

    /// Directorio temporal bajo /tmp: `store_dir` se valida y rechaza rutas con
    /// `.hermes`, que es donde puede caer `TMPDIR` en algunas máquinas.
    fn tmp() -> TempDir {
        TempDir::new_in("/tmp").expect("tempdir en /tmp")
    }

    fn env(installed: &[&str], profiles: &[&str]) -> InitEnv {
        InitEnv {
            installed: installed.iter().map(|s| s.to_string()).collect(),
            hermes_profiles: profiles.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn run(input: &str, dir: &Path, yes: bool, env: &InitEnv) -> (Result<Settings>, String) {
        let mut inp = Cursor::new(input.as_bytes().to_vec());
        let mut out = Vec::new();
        let res = run_init(&mut inp, &mut out, dir, yes, env);
        (res, String::from_utf8(out).expect("utf8"))
    }

    #[test]
    fn yes_accepts_defaults_and_prints_start_command() {
        let dir = tmp();
        let store = dir.path().join("store");
        let (res, out) = run(
            "",
            &store,
            true,
            &env(&["opencode", "hermes"], &["default", "x"]),
        );
        let s = res.expect("init --yes");
        assert_eq!(s.orchestrator_agent.as_deref(), Some("opencode"));
        assert!(s.agent_models.is_empty(), "defaults del código no se fijan");
        assert!(s.hidden_profiles.is_empty());
        assert!(
            out.contains("[ok] opencode") && out.contains("[--] claude"),
            "{out}"
        );
        assert!(out.contains("Para arrancar: forja --store-dir"), "{out}");
        assert!(out.contains("serve --open"));
        assert!(store.join("settings.json").is_file());
    }

    #[test]
    fn interactive_answers_choose_agent_models_and_hidden_profiles() {
        let dir = tmp();
        let e = env(
            &["claude", "agy", "hermes"],
            &["default", "claudia", "privado"],
        );
        // agente, modelo claude, modelo agy (Enter), ocultos, perfil de hermes (no aplica)
        let (res, out) = run("hermes\nopus\n\nprivado\nclaudia\n", dir.path(), false, &e);
        let s = res.expect("init");
        assert_eq!(s.orchestrator_agent.as_deref(), Some("hermes"));
        assert_eq!(
            s.agent_models.get("claude").map(String::as_str),
            Some("opus")
        );
        assert!(
            !s.agent_models.contains_key("agy"),
            "Enter = default del código"
        );
        assert_eq!(s.hidden_profiles, vec!["privado"]);
        assert_eq!(s.orchestrator_profile.as_deref(), Some("claudia"));
        assert!(
            out.contains("Modelo para claude (default del código: sonnet) [sonnet]"),
            "{out}"
        );
        // El oculto no se ofrece como perfil.
        assert!(out.contains("(default/claudia)"), "{out}");
        assert!(!out.contains("default/claudia/privado"));
        let back = load_settings(&dir.path().join("settings.json"))
            .expect("leer")
            .expect("existe");
        assert_eq!(back, s);
    }

    #[test]
    fn invalid_answers_fall_back_to_defaults_without_looping() {
        let dir = tmp();
        let e = env(&["claude", "opencode"], &[]);
        let (res, out) = run("gpt\n--malo\n", dir.path(), false, &e);
        let s = res.expect("init");
        assert_eq!(s.orchestrator_agent.as_deref(), Some("claude"));
        assert!(out.contains("no está instalado"), "{out}");
        assert!(out.contains("modelo no válido"), "{out}");
        assert!(s.agent_models.is_empty());
    }

    #[test]
    fn eof_input_accepts_defaults() {
        let dir = tmp();
        let (res, _) = run("", dir.path(), false, &env(&["agy"], &[]));
        assert_eq!(
            res.expect("init").orchestrator_agent.as_deref(),
            Some("agy")
        );
    }

    #[test]
    fn no_agents_installed_leaves_orchestrator_automatic() {
        let dir = tmp();
        let (res, out) = run("", dir.path(), false, &InitEnv::default());
        let s = res.expect("init");
        assert_eq!(s.orchestrator_agent, None);
        assert!(out.contains("No hay ningún agente instalado"), "{out}");
        assert!(dir.path().join("settings.json").is_file());
    }

    #[test]
    fn existing_settings_are_merged_not_clobbered_and_become_defaults() {
        let dir = tmp();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"max_concurrency":5,"store_dir":"/tmp/x","port":8123,"quota_path":"/tmp/q.json",
                "hide_mascot":true,"agent_models":{"claude":"haiku","agy":"g-9"},
                "orchestrator_agent":"agy","hidden_profiles":["viejo"]}"#,
        )
        .expect("escribir");
        let e = env(&["claude", "agy", "hermes"], &["default", "viejo", "nuevo"]);
        // Todo Enter: los valores existentes son los defaults. Se añade "nuevo" a ocultos.
        let (res, _) = run("\n\n\nviejo, nuevo\n", dir.path(), false, &e);
        let s = res.expect("init");
        assert_eq!(s.max_concurrency, 5);
        assert_eq!(s.port, 8123);
        assert!(s.hide_mascot);
        assert_eq!(s.orchestrator_agent.as_deref(), Some("agy"));
        assert_eq!(s.agent_models["claude"], "haiku");
        assert_eq!(s.agent_models["agy"], "g-9");
        assert_eq!(s.hidden_profiles, vec!["viejo", "nuevo"]);
    }

    #[test]
    fn hidden_protection_is_never_removed_by_init() {
        let dir = tmp();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"max_concurrency":2,"store_dir":"/tmp/x","port":7777,"quota_path":"/tmp/q.json",
                "hidden_profiles":["privado"]}"#,
        )
        .expect("escribir");
        let e = env(&["hermes"], &["default", "privado", "otro"]);
        // agente (Enter=hermes), ocultos: respuesta explícita distinta, perfil: Enter.
        let (res, _) = run("\n-\n\n", dir.path(), false, &e);
        assert!(res
            .expect("init")
            .hidden_profiles
            .contains(&"privado".to_string()));
    }

    #[test]
    fn broken_settings_file_aborts_without_overwriting() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{roto").expect("escribir");
        let (res, _) = run("", dir.path(), true, &env(&["claude"], &[]));
        assert!(res.is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("leer"), "{roto");
    }

    #[test]
    fn start_command_mentions_store_dir_only_when_custom() {
        assert_eq!(
            start_command(Path::new(".forja/store")),
            "forja serve --open"
        );
        assert_eq!(
            start_command(Path::new("/tmp/s")),
            "forja --store-dir /tmp/s serve --open"
        );
    }
}
