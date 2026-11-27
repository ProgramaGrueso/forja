//! Backends del orquestador (Hermes con sesión y CLIs one-shot) y resolución de
//! agente/perfil activo.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use forja_agent::hermes::ProfileRules;
use forja_agent::{Agent, AgyAgent, ClaudeAgent, LocalAgent, OpencodeAgent, RunContext, Runner};
use forja_core::{AgentKind, Privacy, Task};

/// Variable de entorno con el perfil de Hermes que hace de orquestador.
pub const PROFILE_ENV: &str = "FORJA_ORCHESTRATOR_PROFILE";
/// Variable de entorno con el agente que hace de orquestador.
pub const AGENT_ENV: &str = "FORJA_ORCHESTRATOR_AGENT";
/// Agentes que pueden hacer de orquestador.
pub const ORCHESTRATOR_AGENTS: [&str; 5] = ["hermes", "claude", "opencode", "agy", "local"];
/// Orden de la elección automática: el primero instalado gana.
pub const AUTO_ORDER: [&str; 5] = ["claude", "opencode", "agy", "local", "hermes"];
const TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, PartialEq)]
pub struct ChatReply {
    pub text: String,
    pub session_id: Option<String>,
}

/// Opciones por conversación que sobrescriben los ajustes solo para el orquestador.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatOpts {
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub type ChatFuture<'a> = Pin<Box<dyn Future<Output = Result<ChatReply, String>> + Send + 'a>>;

/// Motor de conversación; se sustituye por un doble en las pruebas.
pub trait ChatBackend: Send + Sync {
    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_>;

    /// Igual que [`Self::chat`] con modelo y esfuerzo de la conversación. Por defecto
    /// los ignora (dobles de prueba).
    fn chat_with(
        &self,
        prompt: String,
        resume: Option<String>,
        _opts: &ChatOpts,
    ) -> ChatFuture<'_> {
        self.chat(prompt, resume)
    }

    /// Perfil activo (None si el backend no tiene perfiles).
    fn profile(&self) -> Option<String> {
        None
    }

    /// Cambia el perfil en caliente (no-op si el backend no tiene perfiles).
    fn set_profile(&self, _profile: &str) {}

    /// Agente que hay detrás (`hermes`, `claude`, ...; `custom` en los dobles de prueba).
    fn agent_name(&self) -> String {
        "custom".to_string()
    }

    /// `true` si el backend no conserva sesión entre llamadas: el orquestador
    /// reenvía entonces el historial reciente con cada mensaje.
    fn stateless(&self) -> bool {
        false
    }

    /// Modelos por agente de los ajustes (no-op si el backend no usa modelos).
    fn set_models(&self, _models: &BTreeMap<String, String>) {}

    /// Esfuerzos por agente de los ajustes (no-op si el backend no los usa).
    fn set_efforts(&self, _efforts: &BTreeMap<String, String>) {}
}

/// Backend real: `hermes -p <perfil> chat --query-file <f> -Q --oneshot [--resume <id>]`.
pub struct HermesBackend {
    profile: RwLock<String>,
    rules: ProfileRules,
    program: String,
    models: RwLock<BTreeMap<String, String>>,
    efforts: RwLock<BTreeMap<String, String>>,
}

impl HermesBackend {
    pub fn from_env() -> Self {
        Self::new(ProfileRules::new())
    }

    pub fn new(rules: ProfileRules) -> Self {
        Self {
            profile: RwLock::new(resolve_profile(None, env_profile().as_deref())),
            rules,
            program: "hermes".to_string(),
            models: RwLock::new(BTreeMap::new()),
            efforts: RwLock::new(BTreeMap::new()),
        }
    }

    /// Cambia el programa a ejecutar (los tests apuntan a un script falso).
    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }
}

/// Perfil del orquestador: ajustes > env `FORJA_ORCHESTRATOR_PROFILE` > `default`.
pub fn resolve_profile(setting: Option<&str>, env: Option<&str>) -> String {
    let nonblank = |s: &&str| !s.trim().is_empty();
    setting
        .filter(nonblank)
        .or(env.filter(nonblank))
        .unwrap_or("default")
        .trim()
        .to_string()
}

pub(super) fn env_profile() -> Option<String> {
    std::env::var(PROFILE_ENV).ok()
}

pub fn build_argv(profile: &str, query_file: &Path, resume: Option<&str>) -> Vec<String> {
    build_argv_with(profile, query_file, resume, None, None)
}

/// Como [`build_argv`] con `-m <modelo>` y `--reasoning <nivel>` opcionales.
pub fn build_argv_with(
    profile: &str,
    query_file: &Path,
    resume: Option<&str>,
    model: Option<&str>,
    reasoning: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![
        "hermes".to_string(),
        "-p".to_string(),
        profile.to_string(),
        "chat".to_string(),
        "--query-file".to_string(),
        query_file.to_string_lossy().into_owned(),
        "-Q".to_string(),
        "--oneshot".to_string(),
    ];
    if let Some(id) = resume {
        argv.push("--resume".to_string());
        argv.push(id.to_string());
    }
    if let Some(m) = model {
        argv.push("-m".to_string());
        argv.push(m.to_string());
    }
    if let Some(r) = reasoning {
        argv.push("--reasoning".to_string());
        argv.push(r.to_string());
    }
    argv
}

/// `session_id: X` va por stderr con `-Q`.
pub fn parse_session_id(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .filter_map(|l| l.trim().strip_prefix("session_id:"))
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

impl ChatBackend for HermesBackend {
    fn profile(&self) -> Option<String> {
        self.profile.read().ok().map(|p| p.clone())
    }

    fn set_profile(&self, profile: &str) {
        if let Ok(mut p) = self.profile.write() {
            *p = profile.to_string();
        }
    }

    fn agent_name(&self) -> String {
        "hermes".to_string()
    }

    fn set_models(&self, models: &BTreeMap<String, String>) {
        if let Ok(mut m) = self.models.write() {
            *m = models.clone();
        }
    }

    fn set_efforts(&self, efforts: &BTreeMap<String, String>) {
        if let Ok(mut m) = self.efforts.write() {
            *m = efforts.clone();
        }
    }

    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
        self.chat_with(prompt, resume, &ChatOpts::default())
    }

    fn chat_with(&self, prompt: String, resume: Option<String>, opts: &ChatOpts) -> ChatFuture<'_> {
        let opts = opts.clone();
        Box::pin(async move {
            let (model, effort) = {
                let models = self.models.read().map(|m| m.clone()).unwrap_or_default();
                let efforts = self.efforts.read().map(|m| m.clone()).unwrap_or_default();
                (
                    forja_agent::resolve_model(AgentKind::Hermes, opts.model.as_deref(), &models),
                    forja_agent::resolve_effort(
                        AgentKind::Hermes,
                        opts.effort.as_deref(),
                        &efforts,
                    ),
                )
            };
            let model = match model {
                Some(m) => Some(forja_agent::validate_model(&m).map_err(|e| e.to_string())?),
                None => None,
            };
            let effort = match effort {
                Some(e) => Some(
                    forja_agent::validate_effort(AgentKind::Hermes, &e)
                        .map_err(|e| e.to_string())?,
                ),
                None => None,
            };
            let wanted = self.profile().unwrap_or_default();
            let profile = self.rules.validate(&wanted).map_err(|e| e.to_string())?;
            let dir = private_orchestrator_dir()?;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let file = dir.join(format!("forja-orq-{}-{nanos}.txt", std::process::id()));
            // 0600 y create_new: la consulta (mensajes del usuario) no la lee nadie más.
            if let Err(e) = forja_agent::hermes::write_private_file(&file, prompt.as_bytes()).await
            {
                let _ = std::fs::remove_file(&file);
                return Err(format!("no se pudo escribir la consulta: {e}"));
            }
            let mut argv = build_argv_with(
                &profile,
                &file,
                resume.as_deref(),
                model.as_deref(),
                effort.as_deref(),
            );
            argv[0] = self.program.clone();
            let result = Runner::run_argv(&argv, &dir, TIMEOUT_SECS).await;
            let _ = std::fs::remove_file(&file);
            let out = result.map_err(|e| e.to_string())?;
            let text = out.stdout.trim().to_string();
            if text.is_empty() {
                return Err(format!(
                    "el orquestador no respondió (código {:?}): {}",
                    out.exit_code,
                    out.stderr.trim()
                ));
            }
            Ok(ChatReply {
                text,
                session_id: parse_session_id(&out.stderr),
            })
        })
    }
}

/// Backend one-shot para claude, opencode, agy y local: reutiliza los agentes de
/// `forja-agent` (mismo argv y parseo que en una tarea) con el modelo resuelto
/// desde `agent_models`. No guarda sesión: cada llamada lleva el contexto.
pub struct CliBackend {
    kind: String,
    models: RwLock<BTreeMap<String, String>>,
    efforts: RwLock<BTreeMap<String, String>>,
    /// Programa alternativo (tests con scripts falsos).
    program: Option<String>,
}

impl CliBackend {
    pub fn new(kind: &str, models: BTreeMap<String, String>) -> Self {
        Self {
            kind: kind.to_string(),
            models: RwLock::new(models),
            efforts: RwLock::new(BTreeMap::new()),
            program: None,
        }
    }

    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = Some(program.into());
        self
    }

    /// Agente de `forja-agent` con el modelo efectivo (ajustes > default en código).
    pub fn make_agent(&self) -> Result<Arc<dyn Agent>, String> {
        self.make_agent_with(&ChatOpts::default())
    }

    /// Modelo: conversación > ajustes > default. Esfuerzo: conversación > ajustes > ninguno.
    pub fn make_agent_with(&self, opts: &ChatOpts) -> Result<Arc<dyn Agent>, String> {
        let kind: AgentKind = self
            .kind
            .parse()
            .map_err(|_| format!("agente de orquestador desconocido: {}", self.kind))?;
        let models = self.models.read().map(|m| m.clone()).unwrap_or_default();
        let efforts = self.efforts.read().map(|m| m.clone()).unwrap_or_default();
        let model_override = match opts.model.as_deref() {
            Some(m) => Some(forja_agent::validate_model(m).map_err(|e| e.to_string())?),
            None => None,
        };
        let effort = match forja_agent::resolve_effort(kind, opts.effort.as_deref(), &efforts) {
            Some(e) => Some(forja_agent::validate_effort(kind, &e).map_err(|e| e.to_string())?),
            None if opts.effort.is_some() => {
                return Err(format!(
                    "el agente {kind} no admite esfuerzo de razonamiento"
                ))
            }
            None => None,
        };
        let model = forja_agent::resolve_model(kind, model_override.as_deref(), &models)
            .ok_or_else(|| format!("el agente {kind} no puede ser orquestador"))?;
        let program = self.program.clone();
        let agent: Arc<dyn Agent> = match kind {
            AgentKind::Claude => {
                let a = ClaudeAgent::default().with_model(model);
                Arc::new(match program {
                    Some(p) => a.with_program(p),
                    None => a,
                })
            }
            AgentKind::Opencode => {
                let a = OpencodeAgent::new(model);
                Arc::new(match program {
                    Some(p) => a.with_program(p),
                    None => a,
                })
            }
            AgentKind::Agy => {
                let a = AgyAgent::new(model, forja_agent::agy::DEFAULT_MODE);
                Arc::new(match program {
                    Some(p) => a.with_binary(p),
                    None => a,
                })
            }
            AgentKind::Local => {
                let a = LocalAgent::new(model);
                Arc::new(match program {
                    Some(p) => a.with_program(p),
                    None => a,
                })
            }
            AgentKind::Hermes | AgentKind::Mock => {
                return Err(format!(
                    "el agente {kind} no puede ser orquestador one-shot"
                ))
            }
        };
        match effort {
            Some(e) => agent
                .with_effort(&e)
                .ok_or_else(|| format!("el agente {kind} no admite esfuerzo de razonamiento")),
            None => Ok(agent),
        }
    }
}

impl ChatBackend for CliBackend {
    fn agent_name(&self) -> String {
        self.kind.clone()
    }

    fn stateless(&self) -> bool {
        true
    }

    fn set_models(&self, models: &BTreeMap<String, String>) {
        if let Ok(mut m) = self.models.write() {
            *m = models.clone();
        }
    }

    fn set_efforts(&self, efforts: &BTreeMap<String, String>) {
        if let Ok(mut m) = self.efforts.write() {
            *m = efforts.clone();
        }
    }

    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
        self.chat_with(prompt, resume, &ChatOpts::default())
    }

    fn chat_with(
        &self,
        prompt: String,
        _resume: Option<String>,
        opts: &ChatOpts,
    ) -> ChatFuture<'_> {
        let opts = opts.clone();
        Box::pin(async move {
            let agent = self.make_agent_with(&opts)?;
            // Directorio propio: el agente trabaja ahí, no en el repo ni en /tmp suelto.
            let dir = private_orchestrator_dir()?;
            let ctx = RunContext {
                task: Task::new("orquestador", prompt, Privacy::Open),
                worktree: dir,
                timeout_secs: TIMEOUT_SECS,
            };
            let out = agent.run(&ctx).await.map_err(|e| e.to_string())?;
            let text = out.text.trim().to_string();
            if text.is_empty() {
                return Err(format!(
                    "el orquestador ({}) no respondió (código {:?}): {}",
                    self.kind,
                    out.exit_code,
                    out.raw_stderr.trim()
                ));
            }
            Ok(ChatReply {
                text,
                session_id: None,
            })
        })
    }
}

/// ¿Está instalado el CLI de este agente? (solo mira el `PATH`).
pub fn agent_installed(agent: &str) -> bool {
    use forja_agent::claude::program_available as on_path;
    match agent {
        "claude" | "opencode" | "agy" | "hermes" => on_path(agent),
        "local" => on_path("llm") && on_path("opencode"),
        _ => false,
    }
}

/// Agentes de orquestador instalados, en [`AUTO_ORDER`].
pub fn installed_agents(probe: &dyn Fn(&str) -> bool) -> Vec<String> {
    AUTO_ORDER
        .iter()
        .filter(|a| probe(a))
        .map(|a| a.to_string())
        .collect()
}

/// Agente elegido y de dónde sale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChoice {
    pub agent: String,
    /// `settings` | `env` | `auto`
    pub source: &'static str,
}

fn valid_agent(raw: Option<&str>) -> Option<String> {
    raw.map(|a| a.trim().to_lowercase())
        .filter(|a| ORCHESTRATOR_AGENTS.contains(&a.as_str()))
}

/// Agente del orquestador: ajustes > env `FORJA_ORCHESTRATOR_AGENT` > el primero
/// instalado en [`AUTO_ORDER`] (si no hay ninguno, `hermes`, como siempre).
pub fn resolve_agent(
    setting: Option<&str>,
    env: Option<&str>,
    probe: &dyn Fn(&str) -> bool,
) -> AgentChoice {
    if let Some(agent) = valid_agent(setting) {
        return AgentChoice {
            agent,
            source: "settings",
        };
    }
    if let Some(agent) = valid_agent(env) {
        return AgentChoice {
            agent,
            source: "env",
        };
    }
    let agent = installed_agents(probe)
        .into_iter()
        .next()
        .unwrap_or_else(|| "hermes".to_string());
    AgentChoice {
        agent,
        source: "auto",
    }
}

pub(super) fn env_agent() -> Option<String> {
    std::env::var(AGENT_ENV).ok()
}

/// Cómo se construyen los backends del orquestador y qué agentes están instalados.
#[derive(Clone)]
pub struct BackendFactory {
    pub rules: ProfileRules,
    pub probe: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Programa alternativo por agente (scripts falsos en tests).
    pub programs: BTreeMap<String, String>,
}

impl BackendFactory {
    pub fn system(rules: ProfileRules) -> Self {
        Self {
            rules,
            probe: Arc::new(agent_installed),
            programs: BTreeMap::new(),
        }
    }

    pub fn with_probe(mut self, probe: impl Fn(&str) -> bool + Send + Sync + 'static) -> Self {
        self.probe = Arc::new(probe);
        self
    }

    pub fn with_program(mut self, agent: &str, program: impl Into<String>) -> Self {
        self.programs.insert(agent.to_string(), program.into());
        self
    }

    pub fn build(&self, agent: &str, models: &BTreeMap<String, String>) -> Arc<dyn ChatBackend> {
        if agent == "hermes" {
            let b = HermesBackend::new(self.rules.clone());
            return Arc::new(match self.programs.get("hermes") {
                Some(p) => b.with_program(p.clone()),
                None => b,
            });
        }
        let b = CliBackend::new(agent, models.clone());
        Arc::new(match self.programs.get(agent) {
            Some(p) => b.with_program(p.clone()),
            None => b,
        })
    }
}

/// Carpeta temporal propia del orquestador: `TMPDIR/forja-orquestador-<uid>`,
/// creada con permisos 0700. Si ya existe debe ser un directorio real (no un
/// enlace), de este usuario y sin permisos para grupo ni otros; si no, error:
/// otro usuario podría haberla preparado para leer o cambiar lo que se escribe.
pub(crate) fn private_orchestrator_dir() -> Result<std::path::PathBuf, String> {
    let name = match forja_agent::process::current_uid() {
        Some(uid) => format!("forja-orquestador-{uid}"),
        None => "forja-orquestador".to_string(),
    };
    let dir = std::env::temp_dir().join(name);
    ensure_private_dir(&dir)?;
    Ok(dir)
}

pub(crate) fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    let shown = dir.display();
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("no se pudo crear {shown}: {e}")),
    }
    let meta =
        std::fs::symlink_metadata(dir).map_err(|e| format!("no se pudo comprobar {shown}: {e}"))?;
    if !meta.file_type().is_dir() {
        return Err(format!(
            "{shown} existe y no es un directorio propio (¿enlace?); bórralo o revisa TMPDIR"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if Some(meta.uid()) != forja_agent::process::current_uid() {
            return Err(format!(
                "{shown} pertenece a otro usuario; no se usa. Revisa TMPDIR"
            ));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(format!(
                "{shown} tiene permisos para otros usuarios ({:o}); ejecuta `chmod 700 {shown}`",
                meta.mode() & 0o777
            ));
        }
    }
    Ok(())
}
