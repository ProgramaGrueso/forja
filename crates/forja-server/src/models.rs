//! Detección de los modelos disponibles en esta máquina.
//!
//! Cada fuente es un comando de solo lectura (`agy models`, `opencode models`,
//! `llm list`, `hermes profile list`, `claude --help`). Los parsers son
//! funciones puras para poder probarlos con salidas guardadas.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectedModel {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderModels {
    /// Identificador estable: `anthropic`, `gemini`, `opencode`, `hermes`, `local`.
    pub provider: String,
    pub display_name: String,
    /// Comando del que salió la lista.
    pub source: String,
    /// `ok` | `unavailable`
    pub status: String,
    pub models: Vec<DetectedModel>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelCatalog {
    pub providers: Vec<ProviderModels>,
    pub detected_at_ms: u64,
}

/// Ejecuta un comando y devuelve su salida estándar.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

#[derive(Debug, Clone, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new(program);
        forja_agent::env_guard::scrub_env(&mut command);
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    format!("{program} no está instalado")
                } else {
                    format!("error al ejecutar {program}: {err}")
                }
            })?;

        // Los pipes se vacían en hilos para que una salida larga no bloquee al hijo.
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let err_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });

        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "timeout ejecutando {program} ({}s)",
                        COMMAND_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("error esperando a {program}: {err}"));
                }
            }
        };

        let stdout = out_thread.join().unwrap_or_default();
        let stderr = err_thread.join().unwrap_or_default();
        if !status.success() {
            let text = String::from_utf8_lossy(&stderr).trim().to_string();
            return Err(if text.is_empty() {
                format!("{program} terminó con código {:?}", status.code())
            } else {
                text
            });
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }
}

// ---------------------------------------------------------------- parsers

fn model(id: &str, label: &str) -> DetectedModel {
    DetectedModel {
        id: id.to_string(),
        label: label.to_string(),
    }
}

/// `agy models`: líneas `id<TAB>etiqueta`; ignora el aviso "Fetching…".
pub fn parse_agy_models(text: &str) -> Vec<DetectedModel> {
    text.lines()
        .filter_map(|line| {
            let (id, label) = line.split_once('\t')?;
            let id = id.trim();
            if id.is_empty() || id.contains(' ') {
                return None;
            }
            Some(model(id, label.trim()))
        })
        .collect()
}

/// `opencode models`: líneas `proveedor/modelo`. Se agrupan por proveedor.
pub fn parse_opencode_models(text: &str) -> BTreeMap<String, Vec<DetectedModel>> {
    let mut groups: BTreeMap<String, Vec<DetectedModel>> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((provider, name)) = line.split_once('/') else {
            continue;
        };
        if provider.is_empty() || name.is_empty() || line.contains(' ') {
            continue;
        }
        groups
            .entry(provider.to_string())
            .or_default()
            .push(model(line, name));
    }
    groups
}

/// `llm list`: columnas `alias<TAB>modelo<TAB>contexto<TAB>descripción`.
pub fn parse_llm_list(text: &str) -> Vec<DetectedModel> {
    text.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 2 || cols[0].trim().is_empty() {
                return None;
            }
            let alias = cols[0].trim();
            let desc = cols.get(3).map(|d| d.trim()).filter(|d| !d.is_empty());
            let label = match desc {
                Some(d) => format!("{} · {d}", cols[1].trim()),
                None => cols[1].trim().to_string(),
            };
            Some(model(alias, &label))
        })
        .collect()
}

/// `hermes profile list`: tabla con perfil, modelo y gateway. Omite los perfiles
/// de `hidden` (ajuste `hidden_profiles` y `FORJA_HIDDEN_PROFILES`).
pub fn parse_hermes_profiles(text: &str, hidden: &[String]) -> Vec<DetectedModel> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cleaned = line.trim_start_matches(|c: char| c.is_whitespace() || c == '◆');
        let mut parts = cleaned.split_whitespace();
        let (Some(name), Some(model_id)) = (parts.next(), parts.next()) else {
            continue;
        };
        // Cabecera y separadores de la tabla.
        if name == "Profile" || name.starts_with('─') || model_id.starts_with('─') {
            continue;
        }
        if hidden.iter().any(|h| h.eq_ignore_ascii_case(name)) {
            continue;
        }
        out.push(model(name, model_id));
    }
    out
}

/// `claude --help`: extrae los alias entre comillas del texto de `--model`
/// (por ejemplo `'opus'`, `'sonnet'`). No hay un comando que liste modelos.
pub fn parse_claude_aliases(help: &str) -> Vec<DetectedModel> {
    let Some(start) = help.find("--model <model>") else {
        return Vec::new();
    };
    let block = &help[start + "--model <model>".len()..];
    // El bloque termina en la siguiente opción.
    let end = block.find("\n  -").unwrap_or(block.len());
    let block = &block[..end];

    let mut out: Vec<DetectedModel> = Vec::new();
    let mut parts = block.split('\'');
    parts.next(); // texto antes de la primera comilla
    for (i, piece) in parts.enumerate() {
        // Los índices pares son el contenido entre comillas.
        if i % 2 == 0 {
            let alias = piece.trim();
            let valid = !alias.is_empty()
                && alias.len() <= 24
                && alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if valid && !out.iter().any(|m| m.id == alias) {
                out.push(model(alias, "alias de Claude Code"));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- detección

fn unavailable(provider: &str, display: &str, source: &str, error: String) -> ProviderModels {
    ProviderModels {
        provider: provider.to_string(),
        display_name: display.to_string(),
        source: source.to_string(),
        status: "unavailable".to_string(),
        models: Vec::new(),
        error: Some(error),
    }
}

fn from_result(
    provider: &str,
    display: &str,
    source: &str,
    result: Result<Vec<DetectedModel>, String>,
) -> ProviderModels {
    match result {
        Ok(models) if models.is_empty() => unavailable(
            provider,
            display,
            source,
            "no se detectó ningún modelo".into(),
        ),
        Ok(models) => ProviderModels {
            provider: provider.to_string(),
            display_name: display.to_string(),
            source: source.to_string(),
            status: "ok".to_string(),
            models,
            error: None,
        },
        Err(err) => unavailable(provider, display, source, err),
    }
}

pub fn detect_all(runner: &dyn CommandRunner, hidden: &[String]) -> ModelCatalog {
    let mut providers = Vec::new();

    providers.push(from_result(
        "anthropic",
        "Anthropic (Claude Code)",
        "claude --help",
        runner
            .run("claude", &["--help"])
            .map(|t| parse_claude_aliases(&t)),
    ));

    providers.push(from_result(
        "gemini",
        "Gemini (Antigravity)",
        "agy models",
        runner.run("agy", &["models"]).map(|t| parse_agy_models(&t)),
    ));

    match runner.run("opencode", &["models"]) {
        Ok(text) => {
            let groups = parse_opencode_models(&text);
            if groups.is_empty() {
                providers.push(unavailable(
                    "opencode",
                    "OpenCode",
                    "opencode models",
                    "no se detectó ningún modelo".into(),
                ));
            }
            for (provider, models) in groups {
                providers.push(ProviderModels {
                    provider: format!("opencode:{provider}"),
                    display_name: format!("OpenCode · {provider}"),
                    source: "opencode models".to_string(),
                    status: "ok".to_string(),
                    models,
                    error: None,
                });
            }
        }
        Err(err) => providers.push(unavailable("opencode", "OpenCode", "opencode models", err)),
    }

    providers.push(from_result(
        "hermes",
        "Hermes (perfiles)",
        "hermes profile list",
        runner
            .run("hermes", &["profile", "list"])
            .map(|t| parse_hermes_profiles(&t, hidden)),
    ));

    providers.push(from_result(
        "local",
        "Modelos locales",
        "llm list",
        runner.run("llm", &["list"]).map(|t| parse_llm_list(&t)),
    ));

    ModelCatalog {
        providers,
        detected_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    }
}

/// Catálogo con caché, para no lanzar cinco comandos en cada petición.
pub struct ModelService {
    runner: Arc<dyn CommandRunner>,
    ttl: Duration,
    /// (cuándo, ocultos con los que se calculó, catálogo)
    cache: Mutex<Option<(Instant, Vec<String>, ModelCatalog)>>,
}

impl ModelService {
    pub fn new(runner: Arc<dyn CommandRunner>, ttl: Duration) -> Self {
        Self {
            runner,
            ttl,
            cache: Mutex::new(None),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(Arc::new(SystemRunner), Duration::from_secs(300))
    }

    /// Devuelve el catálogo sin los perfiles de `hidden`; `refresh` ignora la
    /// caché. Si cambia la lista de ocultos la caché se descarta (nunca se
    /// sirve un catálogo calculado con otra lista).
    pub fn catalog(&self, refresh: bool, hidden: &[String]) -> ModelCatalog {
        {
            let guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, cached_hidden, cat)) = guard.as_ref() {
                if !refresh && at.elapsed() < self.ttl && cached_hidden == hidden {
                    return cat.clone();
                }
            }
        }
        let fresh = detect_all(self.runner.as_ref(), hidden);
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some((Instant::now(), hidden.to_vec(), fresh.clone()));
        fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const AGY: &str = "Fetching available models...\n\
gemini-4-neon-high\tGemini 4 Neon (High)\n\
gemini-4-neon-medium\tGemini 4 Neon (Medium)\n\
gemini-4-carbon-low\tGemini 4 Carbon (Low)\n";

    const OPENCODE: &str = "console-google/gemini-4-neon\n\
deepseek/deepseek-flash\n\
llm-local/flashnext-64k\n\
opencode-go/glm-5.3\n\
opencode-go/deepseek-v4.1-flash\n\
línea rara sin barra\n";

    const LLM: &str = "moe\tqwen3.8-35b-a3b-256k\t262144\tQwen3.8 35B-A3B abliterated\n\
flash\tflashnext-64k\t65536\tQwen3.8 Flash-Next\n";

    const HERMES: &str = "\n Profile          Model                        Gateway      Alias        Distribution\n \
───────────────    ───────────────────────────    ───────────    ───────────    ────────────────────\n  \
default         claude-sonnet-5-5            running      —            —\n \
◆claudia         claude-sonnet-5-5[1m]        running      claudia      —\n  \
privado         deepseek-v4.1-flash          running      —            —\n  \
secreto         deepseek-flash               running      secreto      —\n  \
yuuki           gemini-3.8-flash             running      —            —\n";

    const CLAUDE_HELP: &str = "Options:\n  --model <model>                       Model for the current session. Use an\n                                        alias for the latest model (e.g.\n                                        'fable', 'opus', or 'sonnet') or a\n                                        model's full name.\n  --name <name>   Nombre 'ignorado'\n";

    #[test]
    fn agy_parser_skips_notice_and_keeps_labels() {
        let m = parse_agy_models(AGY);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].id, "gemini-4-neon-high");
        assert_eq!(m[0].label, "Gemini 4 Neon (High)");
    }

    #[test]
    fn opencode_parser_groups_by_provider_and_skips_junk() {
        let g = parse_opencode_models(OPENCODE);
        assert_eq!(g.len(), 4);
        assert_eq!(g["opencode-go"].len(), 2);
        assert_eq!(g["opencode-go"][0].id, "opencode-go/glm-5.3");
        assert!(!g.contains_key("línea"));
    }

    #[test]
    fn llm_parser_reads_alias_name_and_description() {
        let m = parse_llm_list(LLM);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].id, "moe");
        assert!(m[0].label.contains("qwen3.8-35b-a3b-256k"));
        assert!(m[0].label.contains("abliterated"));
    }

    #[test]
    fn hermes_parser_hides_configured_profiles_only() {
        let hidden = vec!["privado".to_string(), "secreto".to_string()];
        let m = parse_hermes_profiles(HERMES, &hidden);
        let names: Vec<&str> = m.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(names, vec!["default", "claudia", "yuuki"]);
        assert_eq!(m[1].label, "claude-sonnet-5-5[1m]");

        // Sin ocultos configurados no se esconde nada (no hay nombres fijos en el código).
        let all = parse_hermes_profiles(HERMES, &[]);
        let names: Vec<&str> = all.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(
            names,
            vec!["default", "claudia", "privado", "secreto", "yuuki"]
        );
    }

    #[test]
    fn claude_parser_extracts_aliases_only_from_model_block() {
        let m = parse_claude_aliases(CLAUDE_HELP);
        let ids: Vec<&str> = m.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["fable", "opus", "sonnet"]);
    }

    #[test]
    fn parsers_tolerate_empty_and_garbage_input() {
        assert!(parse_agy_models("").is_empty());
        assert!(parse_opencode_models("???").is_empty());
        assert!(parse_llm_list("sin tabs").is_empty());
        assert!(parse_hermes_profiles("", &[]).is_empty());
        assert!(parse_claude_aliases("nada").is_empty());
    }

    struct FakeRunner {
        outputs: HashMap<&'static str, Result<String, String>>,
        calls: AtomicUsize,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, _args: &[&str]) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outputs
                .get(program)
                .cloned()
                .unwrap_or_else(|| Err(format!("{program} no está instalado")))
        }
    }

    fn fake(outputs: Vec<(&'static str, Result<String, String>)>) -> Arc<FakeRunner> {
        Arc::new(FakeRunner {
            outputs: outputs.into_iter().collect(),
            calls: AtomicUsize::new(0),
        })
    }

    #[test]
    fn detect_all_reports_ok_and_unavailable_per_provider() {
        let runner = fake(vec![
            ("agy", Ok(AGY.to_string())),
            ("llm", Ok(LLM.to_string())),
            ("hermes", Ok(HERMES.to_string())),
            ("claude", Ok(CLAUDE_HELP.to_string())),
            ("opencode", Err("opencode no está instalado".to_string())),
        ]);
        let cat = detect_all(runner.as_ref(), &[]);
        let by: HashMap<&str, &ProviderModels> = cat
            .providers
            .iter()
            .map(|p| (p.provider.as_str(), p))
            .collect();
        assert_eq!(by["gemini"].status, "ok");
        assert_eq!(by["local"].models.len(), 2);
        assert_eq!(by["hermes"].models.len(), 5);
        assert_eq!(by["anthropic"].models.len(), 3);
        assert_eq!(by["opencode"].status, "unavailable");
        assert!(by["opencode"]
            .error
            .as_deref()
            .unwrap()
            .contains("opencode"));
    }

    #[test]
    fn detect_all_splits_opencode_into_one_group_per_provider() {
        let runner = fake(vec![("opencode", Ok(OPENCODE.to_string()))]);
        let cat = detect_all(runner.as_ref(), &[]);
        let groups: Vec<&str> = cat
            .providers
            .iter()
            .map(|p| p.provider.as_str())
            .filter(|p| p.starts_with("opencode:"))
            .collect();
        assert_eq!(
            groups,
            vec![
                "opencode:console-google",
                "opencode:deepseek",
                "opencode:llm-local",
                "opencode:opencode-go"
            ]
        );
    }

    #[test]
    fn service_caches_until_refresh() {
        let runner = fake(vec![("agy", Ok(AGY.to_string()))]);
        let svc = ModelService::new(runner.clone(), Duration::from_secs(60));
        svc.catalog(false, &[]);
        let after_first = runner.calls.load(Ordering::SeqCst);
        svc.catalog(false, &[]);
        assert_eq!(runner.calls.load(Ordering::SeqCst), after_first);
        svc.catalog(true, &[]);
        assert_eq!(runner.calls.load(Ordering::SeqCst), after_first * 2);
    }

    #[test]
    fn service_cache_never_serves_a_list_computed_with_other_hidden() {
        let runner = fake(vec![("hermes", Ok(HERMES.to_string()))]);
        let svc = ModelService::new(runner, Duration::from_secs(60));
        let names = |hidden: &[String]| -> Vec<String> {
            svc.catalog(false, hidden)
                .providers
                .into_iter()
                .find(|p| p.provider == "hermes")
                .map(|p| p.models.into_iter().map(|m| m.id).collect())
                .unwrap_or_default()
        };
        assert!(names(&[]).contains(&"privado".to_string()));
        assert!(!names(&["privado".to_string()]).contains(&"privado".to_string()));
        assert!(names(&[]).contains(&"privado".to_string()));
    }

    #[test]
    fn system_runner_reports_missing_program() {
        let err = SystemRunner
            .run("forja-programa-que-no-existe", &[])
            .unwrap_err();
        assert!(err.contains("no está instalado"), "{err}");
    }
}
