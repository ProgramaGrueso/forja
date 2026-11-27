//! Orquestador conversacional: un agente (Hermes, claude, opencode, agy o local)
//! que habla con el usuario, delega tareas en Forja y revisa sus resultados. El
//! panel lo usa por texto o por voz (Web Speech API en el navegador).
//!
//! Hermes mantiene sesión (`--resume`); los demás CLIs se invocan one-shot y no
//! recuerdan nada, así que el [`Orchestrator`] les reenvía el historial reciente
//! en cada llamada ([`CliBackend`], [`history_block`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use crate::delegation::{self, Delegator, Pending};
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use forja_agent::hermes::ProfileRules;
use forja_agent::{Agent, AgyAgent, ClaudeAgent, LocalAgent, OpencodeAgent, RunContext, Runner};
use forja_core::{AgentKind, Privacy, Task};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::settings::Settings;
use crate::AppState;

/// Variable de entorno con el perfil de Hermes que hace de orquestador.
pub const PROFILE_ENV: &str = "FORJA_ORCHESTRATOR_PROFILE";
/// Variable de entorno con el agente que hace de orquestador.
pub const AGENT_ENV: &str = "FORJA_ORCHESTRATOR_AGENT";
/// Agentes que pueden hacer de orquestador.
pub const ORCHESTRATOR_AGENTS: [&str; 5] = ["hermes", "claude", "opencode", "agy", "local"];
/// Orden de la elección automática: el primero instalado gana.
pub const AUTO_ORDER: [&str; 5] = ["claude", "opencode", "agy", "local", "hermes"];
/// Intercambios (usuario + respuesta) que se reenvían a los CLIs sin sesión.
pub const HISTORY_EXCHANGES: usize = 20;
const HISTORY_MSG_CHARS: usize = 1500;
const HISTORY_TOTAL_CHARS: usize = 30_000;
const TIMEOUT_SECS: u64 = 600;
/// Turnos automáticos seguidos que puede dar una conversación sin que el usuario
/// escriba. Al agotarse, los resultados se dejan para el siguiente mensaje del
/// usuario: así una cadena de delegaciones nunca puede girar sin fin.
pub const MAX_AUTO_TURNS: usize = 8;
/// Espera máxima a que una conversación quede libre antes de dejar la reacción
/// pendiente para el siguiente turno del usuario.
pub const AUTO_IDLE_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

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

fn env_profile() -> Option<String> {
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
            let dir = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let file = dir.join(format!("forja-orq-{}-{nanos}.txt", std::process::id()));
            std::fs::write(&file, &prompt)
                .map_err(|e| format!("no se pudo escribir la consulta: {e}"))?;
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
            let dir = std::env::temp_dir().join("forja-orquestador");
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;
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

fn env_agent() -> Option<String> {
    std::env::var(AGENT_ENV).ok()
}

/// Historial reciente para los CLIs sin sesión: hasta [`HISTORY_EXCHANGES`]
/// intercambios (los mensajes de sistema se omiten), cada mensaje recortado y el
/// total acotado; lo más antiguo se descarta primero. Vacío si no hay historial.
pub fn history_block(messages: &[Message]) -> String {
    let mut lines: Vec<String> = messages
        .iter()
        .filter(|m| m.rol != "sistema")
        .map(|m| {
            let who = if m.rol == "usuario" {
                "Usuario"
            } else {
                "Orquestador"
            };
            let mut text: String = m.texto.chars().take(HISTORY_MSG_CHARS).collect();
            if m.texto.chars().count() > HISTORY_MSG_CHARS {
                text.push('…');
            }
            format!("{who}: {text}")
        })
        .collect();
    let keep = HISTORY_EXCHANGES * 2;
    if lines.len() > keep {
        lines.drain(..lines.len() - keep);
    }
    while lines.len() > 1
        && lines.iter().map(|l| l.chars().count() + 1).sum::<usize>() > HISTORY_TOTAL_CHARS
    {
        lines.remove(0);
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "Historial reciente de la conversación (del más antiguo al más reciente):\n{}\n\n",
        lines.join("\n")
    )
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

/// Instrucciones del primer turno. La respuesta se lee en voz alta, así que debe ser hablada.
/// El `conversation_id` viaja en la URL de `/tasks` para que el servidor sepa a qué
/// conversación avisar cuando termine la tarea delegada.
pub fn system_prompt(port: u16, token: Option<&str>, conversation: &str) -> String {
    let auth = token
        .map(|t| format!(" -H 'Authorization: Bearer {t}'"))
        .unwrap_or_default();
    format!(
        "Eres el orquestador de Forja. Tu función es delegar y revisar, no hacer el trabajo tú.\n\
         Reglas:\n\
         - SIEMPRE respondes al usuario, aunque sea para decir qué estás haciendo.\n\
         - Tu respuesta se lee en voz alta: español natural, frases cortas, sin markdown, sin listas, sin código.\n\
         - Para delegar usa la API local de Forja con curl:\n\
           curl -s -X POST 'http://127.0.0.1:{port}/tasks?conversation_id={conversation}' -H 'Content-Type: application/json'{auth} -d '{{\"titulo\":\"...\",\"prompt\":\"...\",\"privacidad\":\"open\",\"agent\":\"opencode\"}}'\n\
           No quites el parámetro conversation_id: sirve para avisarte cuando termine la tarea.\n\
           agentes: claude, opencode, agy, hermes, local, mock. Privacidad: open o sensitive (sensitive solo admite claude).\n\
         - Revisa el avance con curl http://127.0.0.1:{port}/runs/ID{auth} (estado y salida) y revisa el resultado antes de decir que está listo.\n\
         - Cuando termine una tarea delegada recibirás un aviso automático; revisa el resultado y continúa sin esperar al usuario.\n\
         - Lo trivial lo respondes tú directamente; el trabajo de código se delega.\n\
         - Nunca muestres claves ni tokens.\n\
         Responde al siguiente mensaje del usuario:\n"
    )
}

/// Id de la conversación por defecto (la que usan `/chat` y `/reset` sin `conversation_id`).
pub const PRINCIPAL: &str = "principal";
/// Máximo de conversaciones vivas (incluida la principal).
pub const MAX_CONVERSATIONS: usize = 20;
/// Máximo de mensajes por conversación; al pasarse se descartan los más antiguos.
pub const MAX_MESSAGES: usize = 500;
const MAX_TITLE_CHARS: usize = 60;
const AUTO_TITLE_WORDS: usize = 6;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub rol: String,
    pub texto: String,
    pub ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConversationSummary {
    pub id: String,
    pub titulo: String,
    pub mensajes: usize,
    pub creado: u64,
    pub actualizado: u64,
    /// Hay una llamada a Hermes en curso.
    pub ocupada: bool,
    /// Modelo elegido para esta conversación (None = el de los ajustes).
    pub modelo: Option<String>,
    /// Esfuerzo elegido para esta conversación (None = el de los ajustes).
    pub esfuerzo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConversationDetail {
    #[serde(flatten)]
    pub resumen: ConversationSummary,
    pub historial: Vec<Message>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatError {
    NotFound,
    /// Ya hay una llamada en curso en esa conversación.
    Busy,
    /// Cancelada con `/stop`.
    Cancelled,
    Backend(String),
    /// Modelo o esfuerzo no válido para el agente del orquestador.
    Invalid(String),
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::NotFound => write!(f, "conversación no encontrada"),
            ChatError::Busy => write!(f, "la conversación ya tiene una llamada en curso"),
            ChatError::Cancelled => write!(f, "llamada cancelada"),
            ChatError::Backend(e) | ChatError::Invalid(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    /// Se alcanzó `MAX_CONVERSATIONS`.
    LimitReached,
}

struct InFlight {
    seq: u64,
    cancel: Arc<Notify>,
    /// `true` si es un turno automático (el usuario tiene prioridad).
    auto: bool,
}

struct Conversation {
    title: String,
    session_id: Option<String>,
    messages: Vec<Message>,
    created: u64,
    updated: u64,
    in_flight: Option<InFlight>,
    /// Modelo y esfuerzo propios de la conversación (sobrescriben los ajustes).
    model: Option<String>,
    effort: Option<String>,
    /// Acciones delegadas del último turno, a reinyectar en el siguiente.
    pending: Vec<Pending>,
    /// Turnos automáticos seguidos desde el último mensaje del usuario.
    auto_turns: usize,
}

impl Conversation {
    fn new(title: &str) -> Self {
        let now = now_ms();
        Self {
            title: title.to_string(),
            session_id: None,
            messages: Vec::new(),
            created: now,
            updated: now,
            in_flight: None,
            model: None,
            effort: None,
            pending: Vec::new(),
            auto_turns: 0,
        }
    }

    fn push(&mut self, rol: &str, texto: &str) {
        let ts = now_ms();
        self.messages.push(Message {
            rol: rol.to_string(),
            texto: texto.to_string(),
            ts,
        });
        if self.messages.len() > MAX_MESSAGES {
            let excess = self.messages.len() - MAX_MESSAGES;
            self.messages.drain(..excess);
        }
        self.updated = ts;
    }

    fn summary(&self, id: &str) -> ConversationSummary {
        ConversationSummary {
            id: id.to_string(),
            titulo: self.title.clone(),
            mensajes: self.messages.len(),
            creado: self.created,
            actualizado: self.updated,
            ocupada: self.in_flight.is_some(),
            modelo: self.model.clone(),
            esfuerzo: self.effort.clone(),
        }
    }
}

struct Inner {
    map: HashMap<String, Conversation>,
    counter: u64,
    seq: u64,
    /// Se notifica cuando una conversación deja de estar ocupada.
    idle: Arc<Notify>,
}

/// Seguimiento de las tareas delegadas, para reaccionar cuando terminan.
#[derive(Default)]
struct Reactions {
    /// run_id -> conversación que lo delegó.
    tracked: HashMap<String, String>,
    /// conversación -> run_ids terminados aún no revisados.
    finished: HashMap<String, Vec<String>>,
    /// Conversaciones con una reacción automática encolada o en curso.
    scheduled: HashSet<String>,
}

/// Título automático: primeras palabras del primer mensaje.
pub fn auto_title(message: &str) -> String {
    let words: Vec<&str> = message.split_whitespace().take(AUTO_TITLE_WORDS).collect();
    clamp_title(&words.join(" "))
}

fn clamp_title(title: &str) -> String {
    title.trim().chars().take(MAX_TITLE_CHARS).collect()
}

/// Libera la marca "en curso" aunque la petición HTTP se aborte a mitad.
struct InFlightGuard {
    inner: Arc<Mutex<Inner>>,
    id: String,
    seq: u64,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut g = lock(&self.inner);
        if let Some(c) = g.map.get_mut(&self.id) {
            if c.in_flight.as_ref().is_some_and(|f| f.seq == self.seq) {
                c.in_flight = None;
            }
        }
        let idle = Arc::clone(&g.idle);
        drop(g);
        idle.notify_waiters();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
pub struct Orchestrator {
    backend: Arc<RwLock<Arc<dyn ChatBackend>>>,
    /// Con fábrica, el agente se puede cambiar en caliente; sin ella el backend es fijo.
    factory: Option<BackendFactory>,
    rules: ProfileRules,
    inner: Arc<Mutex<Inner>>,
    reactions: Arc<Mutex<Reactions>>,
}

impl Orchestrator {
    pub fn new(backend: Arc<dyn ChatBackend>) -> Self {
        let mut map = HashMap::new();
        map.insert(PRINCIPAL.to_string(), Conversation::new("Principal"));
        Self {
            backend: Arc::new(RwLock::new(backend)),
            factory: None,
            rules: ProfileRules::new(),
            inner: Arc::new(Mutex::new(Inner {
                map,
                counter: 0,
                seq: 0,
                idle: Arc::new(Notify::new()),
            })),
            reactions: Arc::new(Mutex::new(Reactions::default())),
        }
    }

    /// Reglas de perfiles (descubrimiento y ocultos) para validar en `apply_profile`.
    pub fn with_rules(mut self, rules: ProfileRules) -> Self {
        self.rules = rules.clone();
        let agent = self.agent();
        if let Some(f) = self.factory.as_mut() {
            f.rules = rules;
            let fresh = f.build(&agent, &BTreeMap::new());
            *self.backend.write().unwrap_or_else(|e| e.into_inner()) = fresh;
        }
        self
    }

    /// Orquestador con agente intercambiable. El agente inicial sale de
    /// `FORJA_ORCHESTRATOR_AGENT` o de la detección automática.
    pub fn from_factory(factory: BackendFactory) -> Self {
        let choice = resolve_agent(None, env_agent().as_deref(), &*factory.probe);
        let backend = factory.build(&choice.agent, &BTreeMap::new());
        let mut orq = Self::new(backend).with_rules(factory.rules.clone());
        orq.factory = Some(factory);
        orq
    }

    /// Único punto donde se decide el backend inicial.
    pub fn from_env() -> Self {
        Self::from_factory(BackendFactory::system(ProfileRules::new()))
    }

    pub fn rules(&self) -> &ProfileRules {
        &self.rules
    }

    fn backend(&self) -> Arc<dyn ChatBackend> {
        self.backend
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Agente activo (`hermes`, `claude`, ...).
    pub fn agent(&self) -> String {
        self.backend().agent_name()
    }

    /// Agentes instalados que pueden orquestar (en el orden automático).
    pub fn agents_available(&self) -> Vec<String> {
        match &self.factory {
            Some(f) => installed_agents(&*f.probe),
            None => installed_agents(&agent_installed),
        }
    }

    /// Origen del agente activo según `setting`: `settings` | `env` | `auto`.
    pub fn agent_source(&self, setting: Option<&str>) -> &'static str {
        let probe: &dyn Fn(&str) -> bool = match &self.factory {
            Some(f) => &*f.probe,
            None => &agent_installed,
        };
        resolve_agent(setting, env_agent().as_deref(), probe).source
    }

    /// Perfiles de Hermes elegibles (descubiertos menos ocultos).
    pub fn profiles(&self) -> Vec<String> {
        self.rules.discover()
    }

    /// Comprueba, SIN aplicar nada, que el perfil de los ajustes existe y no está
    /// oculto (con los ocultos de los propios ajustes incluidos).
    pub fn check_settings(&self, s: &Settings) -> Result<(), String> {
        if let Some(profile) = &s.orchestrator_profile {
            self.rules
                .validate_with_extra_hidden(profile, &s.hidden_profiles)
                .map_err(|e| format!("orchestrator_profile: {e}"))?;
        }
        Ok(())
    }

    /// Aplica en caliente ocultos, agente, modelos y perfil. Si cambia el agente o
    /// el perfil se reinician las sesiones. Con ajustes ya validados por
    /// [`Self::check_settings`] solo falla si el perfil desapareció del disco.
    pub fn apply_settings(&self, s: &Settings) -> Result<(), String> {
        self.rules.set_hidden(&s.hidden_profiles);
        if let Some(f) = &self.factory {
            let choice = resolve_agent(
                s.orchestrator_agent.as_deref(),
                env_agent().as_deref(),
                &*f.probe,
            );
            if self.agent() != choice.agent {
                let fresh = f.build(&choice.agent, &s.agent_models);
                *self.backend.write().unwrap_or_else(|e| e.into_inner()) = fresh;
                self.reset_all_sessions();
                // Modelo y esfuerzo por conversación pertenecen a la familia anterior.
                self.clear_conversation_options();
            }
        }
        self.backend().set_models(&s.agent_models);
        self.backend().set_efforts(&s.agent_effort);
        if self.backend().profile().is_some() {
            self.apply_profile(s.orchestrator_profile.as_deref())?;
        }
        Ok(())
    }

    fn clear_conversation_options(&self) {
        for c in lock(&self.inner).map.values_mut() {
            c.model = None;
            c.effort = None;
        }
    }

    /// Sesión de Hermes de la conversación principal.
    pub fn current_session(&self) -> Option<String> {
        self.session_of(PRINCIPAL)
    }

    /// Aplica el perfil de los ajustes (con env y `default` como respaldo). Si el
    /// perfil cambia, se reinicia la sesión. Devuelve el perfil efectivo.
    pub fn apply_profile(&self, setting: Option<&str>) -> Result<String, String> {
        let wanted = resolve_profile(setting, env_profile().as_deref());
        let profile = self.rules.validate(&wanted).map_err(|e| e.to_string())?;
        let backend = self.backend();
        if backend.profile().as_deref() != Some(profile.as_str()) {
            backend.set_profile(&profile);
            self.reset_all_sessions();
        }
        Ok(profile)
    }

    pub fn profile(&self) -> Option<String> {
        self.backend().profile()
    }

    /// Olvida la sesión de Hermes de todas las conversaciones (cambio de perfil o agente).
    fn reset_all_sessions(&self) {
        let mut g = lock(&self.inner);
        for c in g.map.values_mut() {
            if let Some(f) = c.in_flight.take() {
                f.cancel.notify_one();
            }
            c.session_id = None;
            c.auto_turns = 0;
        }
    }

    pub fn session_of(&self, id: &str) -> Option<String> {
        lock(&self.inner)
            .map
            .get(id)
            .and_then(|c| c.session_id.clone())
    }

    /// Crea una conversación; `title` vacío = se titula con el primer mensaje.
    pub fn create(&self, title: Option<&str>) -> Result<ConversationSummary, CreateError> {
        let mut g = lock(&self.inner);
        if g.map.len() >= MAX_CONVERSATIONS {
            return Err(CreateError::LimitReached);
        }
        g.counter += 1;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let mut id = format!("c{}-{:05x}", g.counter, nanos & 0xfffff);
        while g.map.contains_key(&id) {
            g.counter += 1;
            id = format!("c{}-{:05x}", g.counter, nanos & 0xfffff);
        }
        let conv = Conversation::new(&clamp_title(title.unwrap_or("")));
        let summary = conv.summary(&id);
        g.map.insert(id, conv);
        Ok(summary)
    }

    /// Lista por actividad reciente (la más nueva primero).
    pub fn list(&self) -> Vec<ConversationSummary> {
        let g = lock(&self.inner);
        let mut v: Vec<ConversationSummary> = g.map.iter().map(|(id, c)| c.summary(id)).collect();
        v.sort_by(|a, b| {
            b.actualizado
                .cmp(&a.actualizado)
                .then_with(|| a.id.cmp(&b.id))
        });
        v
    }

    pub fn get(&self, id: &str) -> Option<ConversationDetail> {
        let g = lock(&self.inner);
        g.map.get(id).map(|c| ConversationDetail {
            resumen: c.summary(id),
            historial: c.messages.clone(),
        })
    }

    /// Borra una conversación (cancelando su llamada en curso). La principal no
    /// desaparece: se vacía. Devuelve `false` si no existe.
    pub fn delete(&self, id: &str) -> bool {
        let existed = {
            let mut g = lock(&self.inner);
            if id == PRINCIPAL {
                if let Some(c) = g.map.get_mut(PRINCIPAL) {
                    if let Some(f) = c.in_flight.take() {
                        f.cancel.notify_one();
                    }
                    *c = Conversation::new("Principal");
                }
                true
            } else {
                match g.map.remove(id) {
                    Some(c) => {
                        if let Some(f) = c.in_flight {
                            f.cancel.notify_one();
                        }
                        true
                    }
                    None => false,
                }
            }
        };
        if existed {
            self.clear_reactions(id);
        }
        existed
    }

    /// Olvida la sesión de Hermes y el historial, conservando id y título.
    pub fn reset_conversation(&self, id: &str) -> bool {
        let existed = {
            let mut g = lock(&self.inner);
            match g.map.get_mut(id) {
                Some(c) => {
                    if let Some(f) = c.in_flight.take() {
                        f.cancel.notify_one();
                    }
                    c.session_id = None;
                    c.pending.clear();
                    c.messages.clear();
                    c.auto_turns = 0;
                    c.updated = now_ms();
                    true
                }
                None => false,
            }
        };
        if existed {
            self.clear_reactions(id);
        }
        existed
    }

    fn clear_reactions(&self, conversation: &str) {
        let mut r = lock(&self.reactions);
        r.tracked.retain(|_, conv| conv != conversation);
        r.finished.remove(conversation);
        r.scheduled.remove(conversation);
    }

    /// Reinicia la conversación principal.
    pub fn reset(&self) {
        self.reset_conversation(PRINCIPAL);
    }

    pub fn rename(&self, id: &str, title: &str) -> bool {
        let mut g = lock(&self.inner);
        match g.map.get_mut(id) {
            Some(c) => {
                c.title = clamp_title(title);
                c.updated = now_ms();
                true
            }
            None => false,
        }
    }

    /// Cancela la llamada en curso de la conversación. `true` si había algo.
    pub fn stop(&self, id: &str) -> bool {
        let g = lock(&self.inner);
        match g.map.get(id).and_then(|c| c.in_flight.as_ref()) {
            Some(f) => {
                f.cancel.notify_one();
                true
            }
            None => false,
        }
    }

    /// Registra que una ejecución fue delegada desde `conversation`. Devuelve
    /// `false` si la conversación ya no existe (no hay a quién notificar).
    pub fn track_delegation(&self, conversation: &str, run_id: &str) -> bool {
        let exists = lock(&self.inner).map.contains_key(conversation);
        if !exists {
            return false;
        }
        lock(&self.reactions)
            .tracked
            .insert(run_id.to_string(), conversation.to_string());
        true
    }

    /// Marca una ejecución como terminada. Devuelve la conversación solo la
    /// primera vez: llamarlo dos veces (evento SSE y reconciliación) no duplica.
    pub fn note_run_finished(&self, run_id: &str) -> Option<String> {
        let conversation = lock(&self.reactions).tracked.remove(run_id)?;
        // Si la conversación se borró mientras la tarea corría, no hay aviso.
        if !lock(&self.inner).map.contains_key(&conversation) {
            return None;
        }
        let mut r = lock(&self.reactions);
        let ids = r.finished.entry(conversation.clone()).or_default();
        if !ids.iter().any(|id| id == run_id) {
            ids.push(run_id.to_string());
        }
        Some(conversation)
    }

    /// Reclama el derecho a lanzar una reacción automática para la conversación.
    /// Solo una a la vez; devuelve `false` si no hay nada pendiente o ya hay otra.
    pub fn try_schedule_reaction(&self, conversation: &str) -> bool {
        let exists = lock(&self.inner).map.contains_key(conversation);
        if !exists {
            return false;
        }
        let mut r = lock(&self.reactions);
        if r.finished.get(conversation).is_none_or(Vec::is_empty) {
            return false;
        }
        r.scheduled.insert(conversation.to_string())
    }

    /// Run_ids terminados que aún no se han entregado al orquestador.
    pub fn take_reaction_batch(&self, conversation: &str) -> Vec<String> {
        lock(&self.reactions)
            .finished
            .remove(conversation)
            .unwrap_or_default()
    }

    /// Devuelve un lote al estado pendiente (p. ej. si la conversación estaba
    /// ocupada por un turno del usuario y hay que reintentar después).
    pub fn requeue_reaction(&self, conversation: &str, ids: Vec<String>) {
        let mut r = lock(&self.reactions);
        let pending = r.finished.entry(conversation.to_string()).or_default();
        for id in ids {
            if !pending.contains(&id) {
                pending.push(id);
            }
        }
    }

    /// Libera la marca de reacción en curso.
    pub fn release_reaction(&self, conversation: &str) {
        lock(&self.reactions).scheduled.remove(conversation);
    }

    pub fn has_pending_reaction(&self, conversation: &str) -> bool {
        lock(&self.reactions)
            .finished
            .get(conversation)
            .is_some_and(|ids| !ids.is_empty())
    }

    /// Ejecuciones delegadas que el servidor sigue vigilando.
    pub fn tracked_runs(&self) -> Vec<(String, String)> {
        lock(&self.reactions)
            .tracked
            .iter()
            .map(|(run, conversation)| (run.clone(), conversation.clone()))
            .collect()
    }

    /// Quita de «Tareas delegadas» las tareas que ya se entregan en el aviso
    /// automático, para no repetirlas dos veces en el mismo turno.
    pub fn forget_pending_tasks(&self, conversation: &str, ids: &[String]) {
        let mut g = lock(&self.inner);
        if let Some(c) = g.map.get_mut(conversation) {
            c.pending
                .retain(|p| !matches!(p, Pending::Task { id, .. } if ids.iter().any(|i| i == id)));
        }
    }

    /// Añade una nota del sistema al historial (errores del turno automático).
    pub fn push_system(&self, conversation: &str, text: &str) {
        if let Some(c) = lock(&self.inner).map.get_mut(conversation) {
            c.push("sistema", text);
        }
    }

    /// Consume un turno automático si queda margen. Evita cadenas sin fin.
    pub fn begin_auto_turn(&self, conversation: &str) -> bool {
        let mut g = lock(&self.inner);
        match g.map.get_mut(conversation) {
            Some(c) if c.auto_turns < MAX_AUTO_TURNS => {
                c.auto_turns += 1;
                true
            }
            _ => false,
        }
    }

    /// Deshace un turno automático reservado que finalmente no se ejecutó (por
    /// ejemplo, si el usuario ganó la carrera). No debe gastar el límite.
    pub fn abort_auto_turn(&self, conversation: &str) {
        let mut g = lock(&self.inner);
        if let Some(c) = g.map.get_mut(conversation) {
            c.auto_turns = c.auto_turns.saturating_sub(1);
        }
    }

    /// Deja los resultados para el siguiente mensaje del usuario (límite de
    /// turnos automáticos o espera agotada) y lo explica en el historial.
    ///
    /// Solo retira del lote los resultados diferidos: los que hayan llegado
    /// mientras se preparaba la reacción siguen pendientes y el reactor los
    /// recogerá en la siguiente pasada (si no, se perderían o se repetiría la
    /// misma reacción sin fin).
    pub fn defer_reaction(&self, conversation: &str, runs: &[(String, String)]) {
        let notice = format!(
            "Reacción automática pausada para evitar bucles; {} resultado(s) \
             quedan pendientes para tu próximo mensaje.",
            runs.len()
        );
        let mut g = lock(&self.inner);
        if let Some(c) = g.map.get_mut(conversation) {
            for (id, titulo) in runs {
                let already = c
                    .pending
                    .iter()
                    .any(|p| matches!(p, Pending::Task { id: pid, .. } if pid == id));
                if !already {
                    c.pending.push(Pending::Task {
                        id: id.clone(),
                        titulo: titulo.clone(),
                    });
                }
            }
            c.push("sistema", &notice);
        }
        drop(g);
        let mut r = lock(&self.reactions);
        if let Some(pending) = r.finished.get_mut(conversation) {
            pending.retain(|id| !runs.iter().any(|(deferred, _)| deferred == id));
            if pending.is_empty() {
                r.finished.remove(conversation);
            }
        }
    }

    /// Espera a que la conversación no tenga una llamada en curso. `false` si
    /// no existe o se agota `timeout` (no bloquea al vigilante para siempre).
    pub async fn wait_idle(&self, conversation: &str, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let idle = {
                let g = lock(&self.inner);
                match g.map.get(conversation) {
                    None => return false,
                    Some(c) if c.in_flight.is_none() => return true,
                    Some(_) => Arc::clone(&g.idle),
                }
            };
            // `notify_waiters` no guarda permiso: el futuro debe registrarse con
            // `enable` ANTES de la segunda comprobación. Así, si la conversación
            // se libera entre ambas, o el aviso despierta el futuro o la
            // comprobación lo ve, sin ventana de pérdida.
            let notified = idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let g = lock(&self.inner);
                match g.map.get(conversation) {
                    None => return false,
                    Some(c) if c.in_flight.is_none() => return true,
                    Some(_) => {}
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return false;
            }
        }
    }

    /// Modelo y esfuerzo propios de la conversación (None = los de los ajustes).
    pub fn options_of(&self, id: &str) -> Option<(Option<String>, Option<String>)> {
        lock(&self.inner)
            .map
            .get(id)
            .map(|c| (c.model.clone(), c.effort.clone()))
    }

    /// Cambia modelo y/o esfuerzo de la conversación sin reiniciarla. Para cada campo:
    /// `None` = sin cambios, `Some("")` = vuelve a los ajustes, `Some(v)` = lo valida
    /// contra la familia del agente orquestador y lo guarda.
    pub fn update_options(
        &self,
        id: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<(), ChatError> {
        let kind: Option<AgentKind> = self.agent().parse().ok();
        let model = match model.map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(m) => {
                if !kind.is_some_and(|k| forja_agent::MODEL_AGENTS.contains(&k)) {
                    return Err(ChatError::Invalid(
                        "este agente no admite elegir modelo".to_string(),
                    ));
                }
                Some(Some(
                    forja_agent::validate_model(m)
                        .map_err(|e| ChatError::Invalid(e.to_string()))?,
                ))
            }
        };
        let effort = match effort.map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(e) => {
                let kind = kind.ok_or_else(|| {
                    ChatError::Invalid("agente de orquestador desconocido".to_string())
                })?;
                Some(Some(
                    forja_agent::validate_effort(kind, e)
                        .map_err(|e| ChatError::Invalid(e.to_string()))?,
                ))
            }
        };
        let mut g = lock(&self.inner);
        let conv = g.map.get_mut(id).ok_or(ChatError::NotFound)?;
        if let Some(m) = model {
            conv.model = m;
        }
        if let Some(e) = effort {
            conv.effort = e;
        }
        Ok(())
    }

    /// Habla en la conversación principal.
    pub async fn say(
        &self,
        message: &str,
        port: u16,
        token: Option<&str>,
    ) -> Result<String, String> {
        self.say_in(PRINCIPAL, message, port, token)
            .await
            .map_err(|e| e.to_string())
    }

    /// Habla en una conversación. El mutex nunca se mantiene durante la llamada
    /// a Hermes: se clona la sesión, se suelta, se llama y se vuelve a tomar.
    pub async fn say_in(
        &self,
        id: &str,
        message: &str,
        port: u16,
        token: Option<&str>,
    ) -> Result<String, ChatError> {
        self.say_in_with(id, message, port, token, None).await
    }

    /// Como [`Self::say_in`]; con `delegator`, los orquestadores sin sesión (todos menos
    /// Hermes) delegan con bloques `[[forja:...]]` que ejecuta el servidor (ver
    /// [`crate::delegation`]). Hermes conserva el camino `curl`, pero ambos reciben
    /// un turno automático cuando termina una tarea delegada.
    pub async fn say_in_with(
        &self,
        id: &str,
        message: &str,
        port: u16,
        token: Option<&str>,
        delegator: Option<&dyn Delegator>,
    ) -> Result<String, ChatError> {
        self.turn(id, Trigger::User(message), port, token, delegator)
            .await
    }

    /// Turno automático (sin mensaje del usuario) tras terminar una delegación.
    /// El reactor del servidor lo usa para revisar el resultado y continuar.
    pub async fn auto_review(
        &self,
        id: &str,
        notice: &str,
        port: u16,
        token: Option<&str>,
        delegator: Option<&dyn Delegator>,
    ) -> Result<String, ChatError> {
        self.turn(id, Trigger::Auto(notice), port, token, delegator)
            .await
    }

    async fn turn(
        &self,
        id: &str,
        trigger: Trigger<'_>,
        port: u16,
        token: Option<&str>,
        delegator: Option<&dyn Delegator>,
    ) -> Result<String, ChatError> {
        let backend = self.backend();
        let stateless = backend.stateless();
        let delegating = stateless && delegator.is_some();
        let message = trigger.text();
        let is_auto = matches!(trigger, Trigger::Auto(_));
        let (resume, cancel, seq, history, opts, pending) = loop {
            // El mutex se suelta antes de cualquier `await` (el futuro debe ser Send).
            let prepared = {
                let mut g = lock(&self.inner);
                g.seq += 1;
                let seq = g.seq;
                let conv = g.map.get_mut(id).ok_or(ChatError::NotFound)?;
                if let Some(f) = &conv.in_flight {
                    // El usuario manda: si choca con un turno automático, lo cancela
                    // y espera a que se libere en vez de devolverle un 409.
                    if !is_auto && f.auto {
                        f.cancel.notify_one();
                        None
                    } else {
                        return Err(ChatError::Busy);
                    }
                } else {
                    if !is_auto {
                        // Un mensaje del usuario abre una nueva cadena de reacciones.
                        conv.auto_turns = 0;
                    }
                    if conv.title.is_empty() && !is_auto {
                        conv.title = auto_title(message);
                    }
                    // Los CLIs sin sesión reciben el historial previo (sin el mensaje actual).
                    let history = if stateless {
                        history_block(&conv.messages)
                    } else {
                        String::new()
                    };
                    conv.push(if is_auto { "sistema" } else { "usuario" }, message);
                    let cancel = Arc::new(Notify::new());
                    conv.in_flight = Some(InFlight {
                        seq,
                        cancel: cancel.clone(),
                        auto: is_auto,
                    });
                    let resume = if stateless {
                        None
                    } else {
                        conv.session_id.clone()
                    };
                    let pending = if delegating {
                        std::mem::take(&mut conv.pending)
                    } else {
                        Vec::new()
                    };
                    let opts = ChatOpts {
                        model: conv.model.clone(),
                        effort: conv.effort.clone(),
                    };
                    Some((resume, cancel, seq, history, opts, pending))
                }
            };
            match prepared {
                Some(prepared) => break prepared,
                None => {
                    if !self.wait_idle(id, std::time::Duration::from_secs(5)).await {
                        return Err(ChatError::Busy);
                    }
                }
            }
        };
        let pending_block = match delegator {
            Some(d) if delegating => delegation::render_pending(&pending, d).await,
            _ => String::new(),
        };
        let _guard = InFlightGuard {
            inner: self.inner.clone(),
            id: id.to_string(),
            seq,
        };
        let tail = match trigger {
            Trigger::User(text) => format!("Mensaje del usuario:\n{text}"),
            Trigger::Auto(text) => format!(
                "Aviso automático del servidor (no lo escribió el usuario):\n{text}\n\n\
                 Revisa el resultado y continúa sin esperar al usuario."
            ),
        };
        let prompt = if delegating {
            format!(
                "{}{history}{pending_block}{tail}",
                delegation::system_prompt_actions()
            )
        } else if stateless {
            format!("{}{history}{tail}", system_prompt(port, token, id))
        } else if resume.is_some() {
            // Hermes conserva la sesión: el aviso va tal cual.
            match trigger {
                Trigger::User(text) => text.to_string(),
                Trigger::Auto(text) => format!("Aviso automático de Forja: {text}"),
            }
        } else {
            format!("{}{}", system_prompt(port, token, id), tail)
        };
        let outcome = tokio::select! {
            r = backend.chat_with(prompt, resume, &opts) => Some(r),
            _ = cancel.notified() => None,
        };
        // Acciones del orquestador: las ejecuta el servidor ANTES de tomar el mutex.
        let (outcome, new_pending) = match (outcome, delegator) {
            (Some(Ok(mut reply)), Some(d)) if delegating => {
                let (clean, actions) = delegation::parse_actions(&reply.text);
                let had_actions = !actions.is_empty();
                let new_pending = delegation::execute_actions(actions, d).await;
                reply.text = if clean.is_empty() && had_actions {
                    "Hecho, ya estoy con ello.".to_string()
                } else {
                    clean
                };
                (Some(Ok(reply)), new_pending)
            }
            (outcome, _) => (outcome, Vec::new()),
        };
        let mut delegated_runs = Vec::new();
        let mut g = lock(&self.inner);
        let Some(conv) = g.map.get_mut(id) else {
            // Borrada mientras hablaba: no hay dónde guardar nada.
            return match outcome {
                Some(Ok(reply)) => Ok(reply.text),
                Some(Err(e)) => Err(ChatError::Backend(e)),
                None => Err(ChatError::Cancelled),
            };
        };
        let result = match outcome {
            Some(Ok(reply)) => {
                if let Some(sid) = reply.session_id {
                    conv.session_id = Some(sid);
                }
                conv.push("orquestador", &reply.text);
                let delegated: Vec<String> = new_pending
                    .iter()
                    .filter_map(|p| match p {
                        Pending::Task { id, titulo } => Some(format!("{titulo} ({id})")),
                        _ => None,
                    })
                    .collect();
                if !delegated.is_empty() {
                    conv.push(
                        "sistema",
                        &format!("Tareas delegadas: {}", delegated.join(", ")),
                    );
                }
                delegated_runs = new_pending
                    .iter()
                    .filter_map(|p| match p {
                        Pending::Task { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                conv.pending = new_pending;
                Ok(reply.text)
            }
            Some(Err(e)) => {
                // Lo pendiente no se pierde si la llamada falla.
                conv.pending = pending;
                Err(ChatError::Backend(e))
            }
            None => {
                conv.pending = pending;
                conv.push(
                    "sistema",
                    if is_auto {
                        "Turno automático interrumpido por el usuario."
                    } else {
                        "Llamada cancelada con /stop."
                    },
                );
                Err(ChatError::Cancelled)
            }
        };
        drop(g);
        // El servidor vigila estas ejecuciones y despierta al orquestador al acabar.
        if let Some(d) = delegator {
            for run_id in delegated_runs {
                d.track_delegation(id, &run_id);
            }
        }
        result
    }
}

/// Origen de un turno del orquestador: el usuario o el aviso de fin de tarea.
enum Trigger<'a> {
    User(&'a str),
    Auto(&'a str),
}

impl Trigger<'_> {
    fn text(&self) -> &str {
        match self {
            Trigger::User(t) | Trigger::Auto(t) => t,
        }
    }
}

/// Comandos de barra que el servidor resuelve sin llamar a Hermes.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    New,
    Help,
    Rename(String),
    Agents,
    Stop,
    /// `/model [m|reset]`: modelo del orquestador en esta conversación.
    Model(String),
    /// `/effort [nivel|reset]`: esfuerzo de razonamiento en esta conversación.
    Effort(String),
    /// Reservado: lo implementa otro stream.
    Agent(String),
    Unknown(String),
}

/// (comando, descripción) para `/help`.
pub const COMMAND_HELP: &[(&str, &str)] = &[
    ("/new", "crea una conversación nueva"),
    ("/help", "muestra esta lista"),
    ("/rename <título>", "cambia el título de la conversación"),
    ("/agents", "lista los agentes de Forja y su estado"),
    ("/stop", "cancela la llamada en curso de la conversación"),
    (
        "/model <modelo>",
        "cambia el modelo del orquestador en esta conversación (sin argumento: muestra el actual; reset: vuelve a Ajustes)",
    ),
    (
        "/effort <nivel>",
        "cambia el esfuerzo de razonamiento del orquestador en esta conversación (sin argumento: muestra el actual; reset: vuelve a Ajustes)",
    ),
    ("/agent", "disponible pronto"),
];

pub fn help_text() -> String {
    let mut s = String::from("Comandos:");
    for (cmd, desc) in COMMAND_HELP {
        s.push_str(&format!("\n{cmd}: {desc}"));
    }
    s
}

/// Devuelve `None` si el mensaje no es un comando (va normal al orquestador).
pub fn parse_command(message: &str) -> Option<Command> {
    let trimmed = message.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let word = parts.next().unwrap_or_default().to_lowercase();
    let arg = parts.next().unwrap_or_default().trim().to_string();
    Some(match word.as_str() {
        "/new" | "/reset" | "/nueva" => Command::New,
        "/help" | "/ayuda" | "/comandos" => Command::Help,
        "/rename" | "/renombrar" => Command::Rename(arg),
        "/agents" | "/agentes" => Command::Agents,
        "/stop" | "/parar" => Command::Stop,
        "/model" | "/modelo" => Command::Model(arg),
        "/effort" | "/esfuerzo" => Command::Effort(arg),
        "/agent" => Command::Agent(arg),
        _ => Command::Unknown(word),
    })
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Modelo del orquestador para esta conversación (vacío = volver a Ajustes).
    #[serde(default)]
    pub modelo: Option<String>,
    /// Esfuerzo del orquestador para esta conversación (vacío = volver a Ajustes).
    #[serde(default)]
    pub esfuerzo: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatResponse {
    reply: String,
    conversation_id: String,
}

fn conv_id(raw: &Option<String>) -> String {
    raw.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(PRINCIPAL)
        .to_string()
}

fn not_found() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "conversación no encontrada" })),
    )
}

fn limit_reached() -> (StatusCode, Json<Value>) {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": format!("límite de {MAX_CONVERSATIONS} conversaciones; borra alguna")
        })),
    )
}

pub async fn chat(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> (StatusCode, Json<Value>) {
    let message = req.message.trim();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "el mensaje no puede estar vacío"})),
        );
    }
    let id = conv_id(&req.conversation_id);
    let port = state.settings.read().map(|s| s.port).unwrap_or(7777);
    if let Some(cmd) = parse_command(message) {
        return run_command(&state, &id, cmd).await;
    }
    match state
        .orchestrator
        .update_options(&id, req.modelo.as_deref(), req.esfuerzo.as_deref())
    {
        Ok(()) => {}
        Err(ChatError::NotFound) => return not_found(),
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
        }
    }
    match state
        .orchestrator
        .say_in_with(
            &id,
            message,
            port,
            state.auth_token.as_deref(),
            Some(&state),
        )
        .await
    {
        Err(ChatError::Invalid(e)) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))),
        Ok(reply) => (
            StatusCode::OK,
            Json(json!(ChatResponse {
                reply,
                conversation_id: id
            })),
        ),
        Err(ChatError::NotFound) => not_found(),
        Err(ChatError::Busy) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": ChatError::Busy.to_string() })),
        ),
        Err(ChatError::Cancelled) => (
            StatusCode::OK,
            Json(json!({
                "reply": "Llamada cancelada.",
                "conversation_id": id,
                "cancelled": true
            })),
        ),
        Err(ChatError::Backend(error)) => {
            (StatusCode::BAD_GATEWAY, Json(json!({ "error": error })))
        }
    }
}

async fn run_command(state: &AppState, id: &str, cmd: Command) -> (StatusCode, Json<Value>) {
    let orq = &state.orchestrator;
    let ok = |v: Value| (StatusCode::OK, Json(v));
    match cmd {
        Command::New => match orq.create(None) {
            Ok(c) => ok(json!({
                "reply": "Conversación nueva. ¿Qué hacemos?",
                "conversation_id": c.id,
                "new_conversation": true
            })),
            Err(CreateError::LimitReached) => limit_reached(),
        },
        Command::Help => ok(json!({ "reply": help_text(), "conversation_id": id })),
        Command::Rename(title) => {
            if title.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "uso: /rename <título>" })),
                );
            }
            if !orq.rename(id, &title) {
                return not_found();
            }
            ok(json!({
                "reply": format!("Título cambiado a: {}", clamp_title(&title)),
                "conversation_id": id
            }))
        }
        Command::Agents => {
            let runs = state.scheduler.list_runs().await;
            let views = crate::api::build_agent_views(&state.metas, &runs);
            let mut lines = vec!["Agentes de Forja:".to_string()];
            for v in &views {
                lines.push(format!(
                    "{} ({}, {}): {}, {}",
                    v.name,
                    v.provider,
                    v.model,
                    if v.available {
                        "disponible"
                    } else {
                        "no disponible"
                    },
                    v.state
                ));
            }
            ok(json!({
                "reply": lines.join("\n"),
                "conversation_id": id,
                "agents": views
            }))
        }
        Command::Stop => {
            let stopped = orq.stop(id);
            let reply = if stopped {
                "Cancelando la llamada en curso."
            } else {
                "No hay nada en curso en esta conversación."
            };
            ok(json!({ "reply": reply, "conversation_id": id, "stopped": stopped }))
        }
        Command::Model(arg) => option_command(state, id, "modelo", &arg),
        Command::Effort(arg) => option_command(state, id, "esfuerzo", &arg),
        Command::Agent(_) => ok(json!({ "reply": "Disponible pronto.", "conversation_id": id })),
        Command::Unknown(cmd) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("comando desconocido: {cmd}. Usa /help para ver la lista.")
            })),
        ),
    }
}

/// `/model` y `/effort`: sin argumento muestra el valor; `reset` vuelve a los ajustes.
fn option_command(state: &AppState, id: &str, which: &str, arg: &str) -> (StatusCode, Json<Value>) {
    let orq = &state.orchestrator;
    let ok = |reply: String| {
        (
            StatusCode::OK,
            Json(json!({ "reply": reply, "conversation_id": id })),
        )
    };
    let Some((model, effort)) = orq.options_of(id) else {
        return not_found();
    };
    let current = if which == "modelo" { model } else { effort };
    let agent = orq.agent();
    if arg.is_empty() {
        let shown = current.unwrap_or_else(|| "los de Ajustes".to_string());
        return ok(format!("{which} del orquestador ({agent}): {shown}."));
    }
    let reset = matches!(
        arg.to_lowercase().as_str(),
        "reset" | "default" | "ajustes" | "auto"
    );
    let value = if reset { "" } else { arg };
    let res = if which == "modelo" {
        orq.update_options(id, Some(value), None)
    } else {
        orq.update_options(id, None, Some(value))
    };
    match res {
        Ok(()) if reset => ok(format!("{which} restablecido a Ajustes.")),
        Ok(()) => ok(format!(
            "{which} del orquestador ({agent}) cambiado a {}.",
            arg.trim()
        )),
        Err(ChatError::NotFound) => not_found(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ResetRequest {
    #[serde(default)]
    pub conversation_id: Option<String>,
}

pub async fn reset(State(state): State<AppState>, body: Option<Json<ResetRequest>>) -> StatusCode {
    let id = conv_id(&body.and_then(|Json(b)| b.conversation_id));
    if state.orchestrator.reset_conversation(&id) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateRequest {
    #[serde(default, alias = "titulo")]
    pub title: Option<String>,
}

pub async fn list_conversations(State(state): State<AppState>) -> Json<Vec<ConversationSummary>> {
    Json(state.orchestrator.list())
}

pub async fn create_conversation(
    State(state): State<AppState>,
    body: Option<Json<CreateRequest>>,
) -> (StatusCode, Json<Value>) {
    let title = body.and_then(|Json(b)| b.title);
    match state.orchestrator.create(title.as_deref()) {
        Ok(c) => (StatusCode::CREATED, Json(json!(c))),
        Err(CreateError::LimitReached) => limit_reached(),
    }
}

pub async fn get_conversation(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> (StatusCode, Json<Value>) {
    match state.orchestrator.get(&id) {
        Some(d) => (StatusCode::OK, Json(json!(d))),
        None => not_found(),
    }
}

pub async fn delete_conversation(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> StatusCode {
    if state.orchestrator.delete(&id) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Mutex<Vec<(String, Option<String>)>>);

    impl ChatBackend for Fake {
        fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
            Box::pin(async move {
                self.0.lock().unwrap().push((prompt, resume));
                Ok(ChatReply {
                    text: "hecho".into(),
                    session_id: Some("s1".into()),
                })
            })
        }
    }

    /// Doble con perfil, para probar el cambio en caliente.
    struct Profiled(RwLock<String>);

    impl ChatBackend for Profiled {
        fn chat(&self, _prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
            Box::pin(async move {
                Ok(ChatReply {
                    text: "ok".into(),
                    session_id: Some("s1".into()),
                })
            })
        }
        fn profile(&self) -> Option<String> {
            Some(self.0.read().unwrap().clone())
        }
        fn set_profile(&self, p: &str) {
            *self.0.write().unwrap() = p.to_string();
        }
    }

    #[test]
    fn resolve_profile_priority_settings_env_default() {
        assert_eq!(resolve_profile(Some("claudia"), Some("yuuki")), "claudia");
        assert_eq!(resolve_profile(None, Some("yuuki")), "yuuki");
        assert_eq!(resolve_profile(Some("  "), Some("yuuki")), "yuuki");
        assert_eq!(resolve_profile(None, None), "default");
        assert_eq!(resolve_profile(Some(""), Some(" ")), "default");
    }

    /// Base de Hermes falsa con perfiles: los tests nunca leen el ~/.hermes real.
    fn fake_rules(profiles: &[&str]) -> (tempfile::TempDir, ProfileRules) {
        let dir = tempfile::TempDir::new().unwrap();
        for p in profiles {
            std::fs::create_dir_all(dir.path().join("profiles").join(p)).unwrap();
        }
        let rules = ProfileRules::new().with_base(dir.path());
        (dir, rules)
    }

    fn fake_script() -> String {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_orch_agent.sh")
            .to_string_lossy()
            .into_owned()
    }

    fn base_settings() -> Settings {
        Settings::with_defaults("/tmp/forja-test-store")
    }

    #[tokio::test]
    async fn apply_profile_switches_hot_and_resets_session_only_on_change() {
        let (_home, rules) = fake_rules(&["claudia"]);
        let backend = Arc::new(Profiled(RwLock::new("default".into())));
        let orq = Orchestrator::new(backend.clone()).with_rules(rules);
        orq.say("hola", 7777, None).await.unwrap();
        assert!(orq.current_session().is_some());

        // Mismo perfil efectivo: la sesión se conserva (si el env no interfiere).
        if std::env::var(PROFILE_ENV).is_err() {
            assert_eq!(orq.apply_profile(None).unwrap(), "default");
            assert!(orq.current_session().is_some());
        }

        assert_eq!(orq.apply_profile(Some("Claudia")).unwrap(), "claudia");
        assert_eq!(backend.profile().as_deref(), Some("claudia"));
        assert!(orq.current_session().is_none());
    }

    #[test]
    fn apply_profile_rejects_hidden_and_unknown_profiles() {
        let (_home, rules) = fake_rules(&["claudia", "privado"]);
        rules.set_hidden(&["privado".to_string()]);
        let backend = Arc::new(Profiled(RwLock::new("default".into())));
        let orq = Orchestrator::new(backend.clone()).with_rules(rules);
        for bad in ["privado", "otro"] {
            assert!(orq.apply_profile(Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(backend.profile().as_deref(), Some("default"));
        assert!(orq.apply_profile(Some("claudia")).is_ok());
    }

    #[test]
    fn check_settings_uses_discovery_and_the_hidden_of_the_same_settings() {
        let (_home, rules) = fake_rules(&["claudia", "privado"]);
        let orq = Orchestrator::new(Arc::new(Fake(Mutex::new(vec![])))).with_rules(rules);
        let mut s = base_settings();
        s.orchestrator_profile = Some("claudia".into());
        assert!(orq.check_settings(&s).is_ok());
        s.orchestrator_profile = Some("fantasma".into());
        assert!(orq.check_settings(&s).is_err());
        s.orchestrator_profile = Some("privado".into());
        s.hidden_profiles = vec!["privado".into()];
        assert!(orq.check_settings(&s).is_err());
        assert_eq!(orq.profiles(), vec!["default", "claudia", "privado"]);
    }

    fn only(list: &'static [&'static str]) -> impl Fn(&str) -> bool + Send + Sync {
        move |a: &str| list.contains(&a)
    }

    #[test]
    fn resolve_agent_priority_settings_env_auto() {
        let all = only(&["claude", "opencode", "agy", "local", "hermes"]);
        let c = resolve_agent(Some("agy"), Some("local"), &all);
        assert_eq!((c.agent.as_str(), c.source), ("agy", "settings"));
        let c = resolve_agent(Some("  "), Some("Local"), &all);
        assert_eq!((c.agent.as_str(), c.source), ("local", "env"));
        // Valores inválidos se ignoran y se cae a automático.
        let c = resolve_agent(Some("mock"), Some("gpt"), &all);
        assert_eq!((c.agent.as_str(), c.source), ("claude", "auto"));
        // Orden automático: claude, opencode, agy, local, hermes.
        for (installed, want) in [
            (&["hermes", "local", "agy", "opencode"][..], "opencode"),
            (&["hermes", "local", "agy"][..], "agy"),
            (&["hermes", "local"][..], "local"),
            (&["hermes"][..], "hermes"),
            (&[][..], "hermes"),
        ] {
            let probe = move |a: &str| installed.contains(&a);
            assert_eq!(resolve_agent(None, None, &probe).agent, want);
        }
    }

    #[test]
    fn history_block_keeps_last_20_exchanges_skips_system_and_clips() {
        assert_eq!(history_block(&[]), "");
        let mut msgs = Vec::new();
        for i in 0..30 {
            msgs.push(Message {
                rol: "usuario".into(),
                texto: format!("pregunta {i}"),
                ts: 0,
            });
            msgs.push(Message {
                rol: "orquestador".into(),
                texto: format!("respuesta {i}"),
                ts: 0,
            });
        }
        msgs.push(Message {
            rol: "sistema".into(),
            texto: "Llamada cancelada con /stop.".into(),
            ts: 0,
        });
        let h = history_block(&msgs);
        assert!(!h.contains("pregunta 9\n") && !h.contains("respuesta 9\n"));
        assert!(h.contains("Usuario: pregunta 10\n"), "{h}");
        assert!(h.contains("Orquestador: respuesta 29"));
        assert!(!h.contains("cancelada"));
        assert_eq!(h.matches("Usuario:").count(), HISTORY_EXCHANGES);

        let long = Message {
            rol: "usuario".into(),
            texto: "x".repeat(5000),
            ts: 0,
        };
        let h = history_block(&[long]);
        assert!(h.contains('…') && h.chars().count() < 1800);
    }

    #[tokio::test]
    async fn cli_backend_runs_each_agent_one_shot_with_resolved_model() {
        let models: BTreeMap<String, String> = [("claude".to_string(), "opus".to_string())].into();
        for (kind, model) in [
            ("claude", "opus"),
            ("opencode", "opencode-go/deepseek-v4.1-flash"),
            ("agy", "gemini-4-neon-medium"),
        ] {
            let b = CliBackend::new(kind, models.clone()).with_program(fake_script());
            assert!(b.stateless());
            assert_eq!(b.agent_name(), kind);
            let r = b
                .chat("hola \"mundo\"\nlinea 2".to_string(), None)
                .await
                .unwrap();
            assert!(
                r.text.contains(&format!("ECO[modelo={model}]")),
                "{kind}: {}",
                r.text
            );
            assert!(
                r.text.contains("hola \"mundo\"\nlinea 2"),
                "{kind}: {}",
                r.text
            );
            assert_eq!(r.session_id, None);
        }
    }

    #[tokio::test]
    async fn cli_backend_hot_models_missing_program_and_failures() {
        let b = CliBackend::new("claude", BTreeMap::new()).with_program(fake_script());
        let r = b.chat("a".into(), None).await.unwrap();
        assert!(r.text.contains("ECO[modelo=sonnet]"), "{}", r.text);
        b.set_models(&[("claude".to_string(), "haiku".to_string())].into());
        let r = b.chat("a".into(), None).await.unwrap();
        assert!(r.text.contains("ECO[modelo=haiku]"), "{}", r.text);

        let missing = CliBackend::new("opencode", BTreeMap::new())
            .with_program("/no/existe/forja-falso-orquestador");
        assert!(missing.chat("a".into(), None).await.is_err());

        // hermes y mock no son orquestadores one-shot.
        assert!(CliBackend::new("hermes", BTreeMap::new())
            .make_agent()
            .is_err());
        assert!(CliBackend::new("nope", BTreeMap::new())
            .make_agent()
            .is_err());
        // local resuelve su modelo (alias moe) sin ejecutar nada.
        let local = CliBackend::new("local", BTreeMap::new())
            .make_agent()
            .unwrap();
        assert_eq!(local.model().as_deref(), Some("moe"));
    }

    #[tokio::test]
    async fn stateless_backend_gets_history_each_turn_and_never_resumes() {
        let (_home, rules) = fake_rules(&[]);
        let factory = BackendFactory::system(rules)
            .with_probe(only(&["claude"]))
            .with_program("claude", fake_script());
        let orq = Orchestrator::from_factory(factory);
        if std::env::var(AGENT_ENV).is_err() {
            assert_eq!(orq.agent(), "claude");
        }
        orq.apply_settings(&{
            let mut s = base_settings();
            s.orchestrator_agent = Some("claude".into());
            s
        })
        .unwrap();
        let r1 = orq.say("primera pregunta", 7777, Some("tk")).await.unwrap();
        assert!(
            r1.contains("delegar y revisar"),
            "lleva instrucciones: {r1}"
        );
        assert!(r1.contains("Bearer tk"));
        assert!(
            !r1.contains("Historial reciente"),
            "sin historial en el turno 1"
        );
        let r2 = orq.say("segunda pregunta", 7777, None).await.unwrap();
        assert!(
            r2.contains("delegar y revisar"),
            "cada llamada lleva instrucciones"
        );
        assert!(r2.contains("Historial reciente"), "{r2}");
        assert!(r2.contains("Usuario: primera pregunta"));
        assert!(r2.contains("Orquestador: ECO[modelo=sonnet]"));
        assert!(r2.contains("Mensaje del usuario:\nsegunda pregunta"));
        // El mensaje actual no se duplica dentro del historial.
        assert_eq!(r2.matches("Usuario: segunda pregunta").count(), 0);
        assert!(orq.current_session().is_none());
    }

    #[tokio::test]
    async fn apply_settings_switches_agent_hot_and_resets_sessions() {
        let (_home, rules) = fake_rules(&["claudia"]);
        let factory = BackendFactory::system(rules)
            .with_probe(only(&["opencode", "hermes"]))
            .with_program("opencode", fake_script());
        let orq = Orchestrator::from_factory(factory);
        let mut s = base_settings();
        s.orchestrator_agent = Some("opencode".into());
        orq.apply_settings(&s).unwrap();
        assert_eq!(orq.agent(), "opencode");
        assert_eq!(orq.profile(), None);
        assert_eq!(orq.agents_available(), vec!["opencode", "hermes"]);
        let r = orq.say("hola", 7777, None).await.unwrap();
        assert!(
            r.contains("ECO[modelo=opencode-go/deepseek-v4.1-flash]"),
            "{r}"
        );

        // El modelo de los ajustes se aplica en caliente.
        s.agent_models.insert("opencode".into(), "oc/otro".into());
        orq.apply_settings(&s).unwrap();
        let r = orq.say("de nuevo", 7777, None).await.unwrap();
        assert!(r.contains("ECO[modelo=oc/otro]"), "{r}");

        // Cambiar a hermes aplica el perfil y reinicia sesiones.
        s.orchestrator_agent = Some("hermes".into());
        s.orchestrator_profile = Some("claudia".into());
        orq.apply_settings(&s).unwrap();
        assert_eq!(orq.agent(), "hermes");
        assert_eq!(orq.profile().as_deref(), Some("claudia"));

        // Un perfil que no existe falla y no cambia el perfil activo.
        s.orchestrator_profile = Some("fantasma".into());
        assert!(orq.apply_settings(&s).is_err());
        assert_eq!(orq.profile().as_deref(), Some("claudia"));

        // Quitar el ajuste de agente vuelve a automático (opencode instalado).
        s.orchestrator_agent = None;
        s.orchestrator_profile = None;
        if std::env::var(AGENT_ENV).is_err() {
            orq.apply_settings(&s).unwrap();
            assert_eq!(orq.agent(), "opencode");
            assert_eq!(orq.agent_source(None), "auto");
        }
    }

    #[tokio::test]
    async fn hidden_profile_in_settings_is_rejected_by_hermes_backend_at_chat_time() {
        let (_home, rules) = fake_rules(&["privado"]);
        // Programa inexistente: si el rechazo no ocurriera antes, fallaría distinto.
        let backend = HermesBackend::new(rules.clone()).with_program("/no/existe/hermes-falso");
        backend.set_profile("privado");
        rules.set_hidden(&["privado".to_string()]);
        let err = backend.chat("hola".into(), None).await.unwrap_err();
        assert!(err.contains("oculto"), "{err}");
    }

    #[test]
    fn parses_session_id_from_stderr() {
        assert_eq!(
            parse_session_id("\nsession_id: abc-1\n").as_deref(),
            Some("abc-1")
        );
        assert_eq!(parse_session_id("nada"), None);
    }

    #[test]
    fn parses_slash_commands_locally() {
        assert_eq!(parse_command("/new"), Some(Command::New));
        assert_eq!(parse_command("  /NEW  "), Some(Command::New));
        assert_eq!(parse_command("/reset"), Some(Command::New));
        assert_eq!(parse_command("/help"), Some(Command::Help));
        assert_eq!(
            parse_command("/modelo"),
            Some(Command::Model(String::new()))
        );
        assert_eq!(
            parse_command("/effort high"),
            Some(Command::Effort("high".into()))
        );
        assert_eq!(
            parse_command("/esfuerzo"),
            Some(Command::Effort(String::new()))
        );
        assert_eq!(parse_command("hola /new"), None);
    }

    #[test]
    fn argv_has_resume_only_when_given() {
        let f = Path::new("/tmp/q.txt");
        let a = build_argv("claudia", f, None);
        assert_eq!(a[..3], ["hermes", "-p", "claudia"]);
        assert!(!a.contains(&"--resume".to_string()));
        let b = build_argv("claudia", f, Some("s9"));
        assert_eq!(b[b.len() - 2..], ["--resume", "s9"]);
    }

    #[tokio::test]
    async fn first_turn_carries_instructions_then_resumes_session() {
        let fake = Arc::new(Fake(Mutex::new(vec![])));
        let orq = Orchestrator::new(fake.clone());
        assert_eq!(orq.say("hola", 7777, Some("tk")).await.unwrap(), "hecho");
        orq.say("sigue", 7777, Some("tk")).await.unwrap();
        let calls = fake.0.lock().unwrap();
        assert!(calls[0].0.contains("delegar y revisar"));
        assert!(calls[0].0.contains("Bearer tk"));
        // Hermes recibe la pista para que el servidor pueda avisarle al acabar.
        assert!(
            calls[0].0.contains("conversation_id=principal"),
            "{}",
            calls[0].0
        );
        assert!(calls[0].0.ends_with("hola"));
        assert_eq!(calls[0].1, None);
        assert_eq!(calls[1].0, "sigue");
        assert_eq!(calls[1].1.as_deref(), Some("s1"));
        drop(calls);
        orq.reset();
        assert!(orq.current_session().is_none());
    }

    /// Doble que da una sesión distinta por conversación nueva y reutiliza la recibida.
    struct SessionFake {
        n: Mutex<u32>,
        calls: Mutex<Vec<(String, Option<String>)>>,
    }

    impl SessionFake {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                n: Mutex::new(0),
                calls: Mutex::new(vec![]),
            })
        }
    }

    impl ChatBackend for SessionFake {
        fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
            Box::pin(async move {
                let sid = resume.clone().unwrap_or_else(|| {
                    let mut n = self.n.lock().unwrap();
                    *n += 1;
                    format!("sess-{n}")
                });
                self.calls.lock().unwrap().push((prompt, resume));
                Ok(ChatReply {
                    text: "ok".into(),
                    session_id: Some(sid),
                })
            })
        }
    }

    /// Doble que bloquea hasta que `n` llamadas están en vuelo a la vez.
    struct BarrierFake(tokio::sync::Barrier);

    impl ChatBackend for BarrierFake {
        fn chat(&self, _prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
            Box::pin(async move {
                self.0.wait().await;
                Ok(ChatReply {
                    text: "par".into(),
                    session_id: None,
                })
            })
        }
    }

    /// Doble que nunca responde (para `/stop`).
    struct HangFake;

    impl ChatBackend for HangFake {
        fn chat(&self, _prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn conversations_have_isolated_sessions_and_history() {
        let fake = SessionFake::new();
        let orq = Orchestrator::new(fake.clone());
        let a = orq.create(Some("A")).unwrap().id;
        let b = orq.create(Some("B")).unwrap().id;
        orq.say_in(&a, "hola a", 1, None).await.unwrap();
        orq.say_in(&b, "hola b", 1, None).await.unwrap();
        orq.say("hola p", 1, None).await.unwrap();
        let (sa, sb, sp) = (
            orq.session_of(&a).unwrap(),
            orq.session_of(&b).unwrap(),
            orq.current_session().unwrap(),
        );
        assert!(sa != sb && sb != sp && sa != sp);
        orq.say_in(&a, "otra", 1, None).await.unwrap();
        assert_eq!(orq.session_of(&a).unwrap(), sa);
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.last().unwrap().1.as_deref(), Some(sa.as_str()));
        drop(calls);
        let da = orq.get(&a).unwrap();
        assert_eq!(da.historial.len(), 4);
        assert_eq!(da.historial[0].rol, "usuario");
        assert_eq!(da.historial[1].rol, "orquestador");
        assert_eq!(orq.get(&b).unwrap().historial.len(), 2);
        assert_eq!(orq.get(PRINCIPAL).unwrap().historial.len(), 2);
    }

    #[tokio::test]
    async fn two_conversations_talk_at_the_same_time() {
        let orq = Orchestrator::new(Arc::new(BarrierFake(tokio::sync::Barrier::new(2))));
        let a = orq.create(None).unwrap().id;
        let b = orq.create(None).unwrap().id;
        let (o1, o2) = (orq.clone(), orq.clone());
        let work = async move {
            tokio::join!(o1.say_in(&a, "uno", 1, None), o2.say_in(&b, "dos", 1, None))
        };
        // Si un lock se mantuviera durante la llamada, el Barrier nunca se liberaría.
        let (ra, rb) = tokio::time::timeout(std::time::Duration::from_secs(5), work)
            .await
            .expect("las dos conversaciones deben avanzar a la vez");
        assert_eq!(ra.unwrap(), "par");
        assert_eq!(rb.unwrap(), "par");
    }

    #[tokio::test]
    async fn same_conversation_rejects_overlapping_calls_and_stop_cancels() {
        let orq = Orchestrator::new(Arc::new(HangFake));
        let id = orq.create(None).unwrap().id;
        let (o, i) = (orq.clone(), id.clone());
        let task = tokio::spawn(async move { o.say_in(&i, "cuelga", 1, None).await });
        while !orq.get(&id).unwrap().resumen.ocupada {
            tokio::task::yield_now().await;
        }
        assert_eq!(orq.say_in(&id, "otra", 1, None).await, Err(ChatError::Busy));
        assert!(orq.stop(&id));
        assert_eq!(task.await.unwrap(), Err(ChatError::Cancelled));
        let d = orq.get(&id).unwrap();
        assert!(!d.resumen.ocupada);
        assert_eq!(d.historial.last().unwrap().rol, "sistema");
        assert!(!orq.stop(&id), "ya no hay nada en curso");
    }

    #[tokio::test]
    async fn delete_removes_conversation_but_principal_is_only_cleared() {
        let orq = Orchestrator::new(SessionFake::new());
        let a = orq.create(None).unwrap().id;
        orq.say_in(&a, "hola", 1, None).await.unwrap();
        assert!(orq.delete(&a));
        assert!(orq.get(&a).is_none());
        assert!(!orq.delete(&a));
        assert_eq!(orq.say_in(&a, "x", 1, None).await, Err(ChatError::NotFound));
        orq.say("p", 1, None).await.unwrap();
        assert!(orq.delete(PRINCIPAL));
        assert!(orq.current_session().is_none());
        assert_eq!(orq.get(PRINCIPAL).unwrap().historial.len(), 0);
    }

    #[tokio::test]
    async fn limits_on_conversations_and_messages() {
        let orq = Orchestrator::new(SessionFake::new());
        // La principal ya cuenta.
        for _ in 1..MAX_CONVERSATIONS {
            orq.create(None).unwrap();
        }
        assert_eq!(orq.create(None), Err(CreateError::LimitReached));
        assert_eq!(orq.list().len(), MAX_CONVERSATIONS);
        for i in 0..(MAX_MESSAGES / 2 + 10) {
            orq.say(&format!("m{i}"), 1, None).await.unwrap();
        }
        let d = orq.get(PRINCIPAL).unwrap();
        assert_eq!(d.historial.len(), MAX_MESSAGES);
        assert_eq!(d.resumen.mensajes, MAX_MESSAGES);
        assert!(d.historial.last().unwrap().texto == "ok");
        // Se descartaron los más antiguos.
        assert!(d.historial.iter().all(|m| m.texto != "m0"));
    }

    #[tokio::test]
    async fn title_is_automatic_from_first_message_and_rename_wins() {
        let orq = Orchestrator::new(SessionFake::new());
        let a = orq.create(None).unwrap().id;
        assert_eq!(orq.get(&a).unwrap().resumen.titulo, "");
        orq.say_in(&a, "uno dos tres cuatro cinco seis siete ocho", 1, None)
            .await
            .unwrap();
        assert_eq!(
            orq.get(&a).unwrap().resumen.titulo,
            "uno dos tres cuatro cinco seis"
        );
        orq.say_in(&a, "otro mensaje distinto", 1, None)
            .await
            .unwrap();
        assert_eq!(
            orq.get(&a).unwrap().resumen.titulo,
            "uno dos tres cuatro cinco seis"
        );
        assert!(orq.rename(&a, "  Mi título  "));
        assert_eq!(orq.get(&a).unwrap().resumen.titulo, "Mi título");
        assert!(!orq.rename("nope", "x"));
        let named = orq.create(Some("Fijo")).unwrap().id;
        orq.say_in(&named, "hola", 1, None).await.unwrap();
        assert_eq!(orq.get(&named).unwrap().resumen.titulo, "Fijo");
    }

    #[test]
    fn parses_new_commands() {
        assert_eq!(
            parse_command("/rename Mi chat  largo"),
            Some(Command::Rename("Mi chat  largo".into()))
        );
        assert_eq!(parse_command("/rename"), Some(Command::Rename("".into())));
        assert_eq!(parse_command("/agents"), Some(Command::Agents));
        assert_eq!(parse_command("/stop"), Some(Command::Stop));
        assert_eq!(
            parse_command("/model gpt x"),
            Some(Command::Model("gpt x".into()))
        );
        assert_eq!(
            parse_command("/agent claude"),
            Some(Command::Agent("claude".into()))
        );
        let help = help_text();
        for (cmd, _) in COMMAND_HELP {
            assert!(help.contains(cmd));
        }
    }

    mod delegation_flow {
        use super::*;
        use forja_sched::{Scheduler, SchedulerConfig};
        use forja_store::JsonlStore;
        use tempfile::TempDir;

        /// Estado con scheduler real (sin repo) y un orquestador `claude` con el script falso.
        fn setup() -> (AppState, Orchestrator, TempDir, TempDir, TempDir) {
            let s = TempDir::new().unwrap();
            let w = TempDir::new().unwrap();
            let (home, rules) = fake_rules(&[]);
            let store = JsonlStore::new(s.path()).unwrap();
            let sched = Scheduler::new(store, SchedulerConfig::new(w.path())).unwrap();
            let factory = BackendFactory::system(rules)
                .with_probe(only(&["claude"]))
                .with_program("claude", fake_script());
            let orq = Orchestrator::from_factory(factory);
            orq.apply_settings(&{
                let mut st = base_settings();
                st.orchestrator_agent = Some("claude".into());
                st
            })
            .unwrap();
            let state = AppState::new(sched).with_orchestrator(orq.clone());
            (state, orq, s, w, home)
        }

        async fn say(state: &AppState, orq: &Orchestrator, msg: &str) -> String {
            orq.say_in_with(PRINCIPAL, msg, 7777, None, Some(state))
                .await
                .expect("turno")
        }

        // ---- reacción automática al terminar una delegación ----

        #[derive(Clone, Copy)]
        enum ReactionMode {
            /// Delega en el primer turno; después solo revisa.
            DelegateFirst,
            /// Delega en cada llamada hasta `n` (para probar el límite).
            DelegateUntil(usize),
            /// Nunca delega.
            ReplyOnly,
        }

        struct ReactionFake {
            calls: Arc<Mutex<Vec<String>>>,
            mode: ReactionMode,
        }

        fn delegate_block(titulo: &str) -> String {
            format!(
                "Delegando.\n[[forja:delegar {{\"titulo\":\"{titulo}\",\"prompt\":\"p\",\
                 \"agent\":\"mock\",\"privacidad\":\"open\"}}]]"
            )
        }

        impl ChatBackend for ReactionFake {
            fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
                self.chat_with(prompt, resume, &ChatOpts::default())
            }

            fn chat_with(
                &self,
                prompt: String,
                _resume: Option<String>,
                _opts: &ChatOpts,
            ) -> ChatFuture<'_> {
                let calls = self.calls.clone();
                let mode = self.mode;
                Box::pin(async move {
                    let n = {
                        let mut calls = calls.lock().unwrap();
                        calls.push(prompt);
                        calls.len()
                    };
                    let text = match mode {
                        ReactionMode::ReplyOnly => "Revisado, sin cambios.".to_string(),
                        ReactionMode::DelegateFirst if n == 1 => delegate_block("Tarea auto"),
                        ReactionMode::DelegateUntil(limit) if n <= limit => {
                            delegate_block("Tarea en cadena")
                        }
                        _ => "Revisado, sin cambios.".to_string(),
                    };
                    Ok(ChatReply {
                        text,
                        session_id: None,
                    })
                })
            }

            fn stateless(&self) -> bool {
                true
            }

            fn agent_name(&self) -> String {
                "custom".to_string()
            }
        }

        fn reaction_setup(
            mode: ReactionMode,
        ) -> (
            AppState,
            Orchestrator,
            Arc<Mutex<Vec<String>>>,
            TempDir,
            TempDir,
        ) {
            let s = TempDir::new().unwrap();
            let w = TempDir::new().unwrap();
            let store = JsonlStore::new(s.path()).unwrap();
            let sched = Scheduler::new(store, SchedulerConfig::new(w.path())).unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let orq = Orchestrator::new(Arc::new(ReactionFake {
                calls: calls.clone(),
                mode,
            }));
            let state = AppState::new(sched).with_orchestrator(orq.clone());
            state.ensure_run_watcher();
            (state, orq, calls, s, w)
        }

        async fn wait_calls(calls: &Arc<Mutex<Vec<String>>>, n: usize) {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            while tokio::time::Instant::now() < deadline {
                if calls.lock().unwrap().len() >= n {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!(
                "esperaba {n} llamadas al orquestador; hubo {}",
                calls.lock().unwrap().len()
            );
        }

        #[tokio::test]
        async fn finished_delegation_wakes_the_orchestrator_without_a_user_message() {
            let (state, orq, calls, _s, _w) = reaction_setup(ReactionMode::DelegateFirst);
            let first = state
                .orchestrator
                .say_in_with(PRINCIPAL, "hazlo", 7777, None, Some(&state))
                .await
                .unwrap();
            assert_eq!(first, "Delegando.");
            assert_eq!(state.scheduler.list_runs().await.len(), 1);

            wait_calls(&calls, 2).await;
            let calls = calls.lock().unwrap().clone();
            assert_eq!(calls.len(), 2, "{calls:?}");
            assert!(calls[1].contains("Aviso automático"), "{}", calls[1]);
            assert!(calls[1].contains("Tarea auto"), "{}", calls[1]);
            assert!(calls[1].contains("estado: done"), "{}", calls[1]);

            let h = orq.get(PRINCIPAL).unwrap().historial;
            assert!(
                h.iter()
                    .any(|m| m.rol == "sistema" && m.texto.contains("Terminaron tareas")),
                "{h:?}"
            );
            assert!(
                h.iter()
                    .any(|m| m.rol == "orquestador" && m.texto == "Revisado, sin cambios."),
                "{h:?}"
            );
            // No hay un tercer turno: la cadena termina cuando el agente no delega.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            assert_eq!(calls.len(), 2);
        }

        #[tokio::test]
        async fn run_finished_before_tracking_is_reconciled_without_duplicates() {
            let (state, _orq, calls, _s, _w) = reaction_setup(ReactionMode::ReplyOnly);
            let rec = state
                .submit_task(&crate::CreateTaskRequest {
                    titulo: "Ya terminada".into(),
                    prompt: "p".into(),
                    privacidad: Privacy::Open,
                    agent: AgentKind::Mock,
                    modelo: None,
                    esfuerzo: None,
                })
                .await
                .unwrap();
            state.scheduler.wait_run(&rec.id).await.unwrap();
            // El evento terminal ya pasó: registrar ahora debe reconciliar.
            state.track_delegation(PRINCIPAL, &rec.id);
            wait_calls(&calls, 1).await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert_eq!(calls.lock().unwrap().len(), 1, "sin duplicados");
        }

        #[tokio::test]
        async fn post_tasks_with_conversation_id_notifies_hermes_conversation() {
            use axum::body::{to_bytes, Body};
            use axum::http::{header, Method, Request};
            use tower::ServiceExt;

            let (state, orq, calls, _s, _w) = reaction_setup(ReactionMode::ReplyOnly);
            let app = crate::build_router(state);
            let req = Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(Method::POST)
                .uri(format!("/tasks?conversation_id={PRINCIPAL}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"titulo":"Desde Hermes","prompt":"p","agent":"mock"}).to_string(),
                ))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::CREATED);
            let _ = to_bytes(resp.into_body(), usize::MAX).await.unwrap();

            wait_calls(&calls, 1).await;
            let calls = calls.lock().unwrap().clone();
            assert!(calls[0].contains("Aviso automático"), "{}", calls[0]);
            assert!(calls[0].contains("Desde Hermes"), "{}", calls[0]);
            assert!(orq
                .get(PRINCIPAL)
                .unwrap()
                .historial
                .iter()
                .any(|m| { m.rol == "orquestador" && m.texto == "Revisado, sin cambios." }));
        }

        #[tokio::test]
        async fn automatic_turns_are_capped_and_leave_the_result_for_the_user() {
            let limit = 1 + MAX_AUTO_TURNS;
            let (state, orq, calls, _s, _w) = reaction_setup(ReactionMode::DelegateUntil(limit));
            orq.say_in_with(PRINCIPAL, "hazlo", 7777, None, Some(&state))
                .await
                .unwrap();

            wait_calls(&calls, limit).await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            assert_eq!(
                calls.lock().unwrap().len(),
                limit,
                "no debe haber más turnos automáticos que MAX_AUTO_TURNS"
            );
            let h = orq.get(PRINCIPAL).unwrap().historial;
            assert!(
                h.iter()
                    .any(|m| m.rol == "sistema" && m.texto.contains("pausada")),
                "el límite debe explicarse en el historial: {h:?}"
            );

            // El siguiente mensaje del usuario sí recibe el resultado pendiente.
            orq.say_in_with(PRINCIPAL, "¿cómo va?", 7777, None, Some(&state))
                .await
                .unwrap();
            let last = calls.lock().unwrap().last().cloned().unwrap();
            assert!(last.contains("Tareas delegadas"), "{last}");
            assert!(last.contains("Tarea en cadena"), "{last}");
        }

        #[tokio::test]
        async fn reaction_waits_for_a_busy_user_turn_instead_of_failing() {
            struct BusyFake(Arc<Mutex<Vec<String>>>);
            impl ChatBackend for BusyFake {
                fn chat(&self, prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
                    let calls = self.0.clone();
                    Box::pin(async move {
                        calls.lock().unwrap().push(prompt.clone());
                        // El aviso va primero: el historial también contiene "hazlo".
                        if prompt.contains("Aviso automático del servidor") {
                            return Ok(ChatReply {
                                text: "Revisado tras esperar.".into(),
                                session_id: None,
                            });
                        }
                        if prompt.contains("mientras") {
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                            return Ok(ChatReply {
                                text: "Seguimos hablando.".into(),
                                session_id: None,
                            });
                        }
                        Ok(ChatReply {
                            text: delegate_block("Tarea lenta"),
                            session_id: None,
                        })
                    })
                }
                fn stateless(&self) -> bool {
                    true
                }
            }

            let s = TempDir::new().unwrap();
            let w = TempDir::new().unwrap();
            let store = JsonlStore::new(s.path()).unwrap();
            let sched = Scheduler::new(store, SchedulerConfig::new(w.path())).unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let orq = Orchestrator::new(Arc::new(BusyFake(calls.clone())));
            let state = AppState::new(sched)
                .with_agent(Arc::new(
                    forja_agent::MockAgent::success("ok")
                        .with_delay(std::time::Duration::from_millis(120)),
                ))
                .with_orchestrator(orq.clone());
            state.ensure_run_watcher();

            orq.say_in_with(PRINCIPAL, "hazlo", 7777, None, Some(&state))
                .await
                .unwrap();
            // Segundo turno del usuario en curso cuando termina la tarea.
            let o2 = orq.clone();
            let s2 = state.clone();
            let user_turn = tokio::spawn(async move {
                o2.say_in_with(PRINCIPAL, "mientras", 7777, None, Some(&s2))
                    .await
            });
            wait_calls(&calls, 3).await;
            user_turn.await.unwrap().unwrap();
            let calls = calls.lock().unwrap().clone();
            let auto = calls
                .iter()
                .position(|p| p.contains("Aviso automático"))
                .expect("turno automático");
            assert!(
                auto >= 2,
                "la reacción esperó al turno del usuario: {calls:?}"
            );
            // No hubo un error de conversación ocupada.
            assert!(
                orq.get(PRINCIPAL)
                    .unwrap()
                    .historial
                    .iter()
                    .any(|m| m.texto == "Revisado tras esperar."),
                "el turno automático debía completarse"
            );
        }

        #[tokio::test]
        async fn user_message_cancels_an_in_flight_automatic_turn() {
            struct InterruptFake(Arc<Mutex<Vec<String>>>);
            impl ChatBackend for InterruptFake {
                fn chat(&self, prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
                    let calls = self.0.clone();
                    Box::pin(async move {
                        calls.lock().unwrap().push(prompt.clone());
                        if prompt.contains("Aviso automático del servidor") {
                            // Turno automático lento: el usuario llegará antes.
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            return Ok(ChatReply {
                                text: "Revisión automática tardía.".into(),
                                session_id: None,
                            });
                        }
                        if prompt.contains("usuario pregunta") {
                            return Ok(ChatReply {
                                text: "Respuesta al usuario.".into(),
                                session_id: None,
                            });
                        }
                        Ok(ChatReply {
                            text: delegate_block("Tarea rápida"),
                            session_id: None,
                        })
                    })
                }
                fn stateless(&self) -> bool {
                    true
                }
            }

            let s = TempDir::new().unwrap();
            let w = TempDir::new().unwrap();
            let store = JsonlStore::new(s.path()).unwrap();
            let sched = Scheduler::new(store, SchedulerConfig::new(w.path())).unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let orq = Orchestrator::new(Arc::new(InterruptFake(calls.clone())));
            let state = AppState::new(sched).with_orchestrator(orq.clone());
            state.ensure_run_watcher();

            orq.say_in_with(PRINCIPAL, "hazlo", 7777, None, Some(&state))
                .await
                .unwrap();
            wait_calls(&calls, 2).await;

            // El usuario escribe mientras el turno automático está en vuelo.
            let reply = orq
                .say_in_with(PRINCIPAL, "usuario pregunta", 7777, None, Some(&state))
                .await
                .unwrap();
            assert_eq!(reply, "Respuesta al usuario.");
            let h = orq.get(PRINCIPAL).unwrap().historial;
            assert!(
                h.iter().any(|m| m
                    .texto
                    .contains("Turno automático interrumpido por el usuario")),
                "{h:?}"
            );
            assert!(!h.iter().any(|m| m.texto == "Revisión automática tardía."));
        }

        #[tokio::test]
        async fn reaction_bookkeeping_is_idempotent() {
            let orq = Orchestrator::new(Arc::new(ReactionFake {
                calls: Arc::new(Mutex::new(Vec::new())),
                mode: ReactionMode::ReplyOnly,
            }));
            assert!(!orq.track_delegation("nope", "r0"));
            assert!(orq.track_delegation(PRINCIPAL, "r1"));
            assert_eq!(orq.note_run_finished("r1").as_deref(), Some(PRINCIPAL));
            // El evento del planificador y la reconciliación no duplican.
            assert_eq!(orq.note_run_finished("r1"), None);
            assert_eq!(orq.note_run_finished("r2"), None);
            assert!(orq.try_schedule_reaction(PRINCIPAL));
            assert!(
                !orq.try_schedule_reaction(PRINCIPAL),
                "solo una reacción por conversación"
            );
            assert_eq!(orq.take_reaction_batch(PRINCIPAL), vec!["r1".to_string()]);
            orq.release_reaction(PRINCIPAL);
            assert!(!orq.has_pending_reaction(PRINCIPAL));
            // Un turno reservado que no llega a ejecutarse no gasta el límite.
            for _ in 0..MAX_AUTO_TURNS {
                assert!(orq.begin_auto_turn(PRINCIPAL));
            }
            assert!(!orq.begin_auto_turn(PRINCIPAL));
            orq.abort_auto_turn(PRINCIPAL);
            assert!(orq.begin_auto_turn(PRINCIPAL));
        }

        #[tokio::test]
        async fn deferring_keeps_results_that_arrive_meanwhile() {
            let orq = Orchestrator::new(Arc::new(ReactionFake {
                calls: Arc::new(Mutex::new(Vec::new())),
                mode: ReactionMode::ReplyOnly,
            }));
            assert!(orq.track_delegation(PRINCIPAL, "r1"));
            assert_eq!(orq.note_run_finished("r1").as_deref(), Some(PRINCIPAL));
            // El reactor ya tomó el primer lote y va a aplazarlo.
            let batch = orq.take_reaction_batch(PRINCIPAL);
            assert_eq!(batch, vec!["r1".to_string()]);

            // Mientras prepara el aplazamiento termina otra tarea delegada.
            assert!(orq.track_delegation(PRINCIPAL, "r2"));
            assert_eq!(orq.note_run_finished("r2").as_deref(), Some(PRINCIPAL));
            orq.defer_reaction(PRINCIPAL, &[("r1".to_string(), "Uno".to_string())]);

            // El nuevo resultado sigue pendiente y disparará otra reacción.
            assert_eq!(
                orq.take_reaction_batch(PRINCIPAL),
                vec!["r2".to_string()],
                "un resultado que llega durante el aplazamiento no se pierde"
            );
            assert!(
                !orq.has_pending_reaction(PRINCIPAL),
                "el resultado ya aplazado no debe relanzar la reacción"
            );
            let h = orq.get(PRINCIPAL).unwrap().historial;
            assert!(h.iter().any(|m| m.texto.contains("pausada")), "{h:?}");
        }

        #[tokio::test]
        async fn deferring_without_taking_first_does_not_reschedule_forever() {
            let orq = Orchestrator::new(Arc::new(ReactionFake {
                calls: Arc::new(Mutex::new(Vec::new())),
                mode: ReactionMode::ReplyOnly,
            }));
            assert!(orq.track_delegation(PRINCIPAL, "r1"));
            assert_eq!(orq.note_run_finished("r1").as_deref(), Some(PRINCIPAL));
            // Al agotarse el límite, el lote diferido sigue en `finished` hasta
            // que `defer_reaction` lo retira; si no, el reactor lo vería otra vez
            // y se reprogramaría en bucle.
            orq.defer_reaction(PRINCIPAL, &[("r1".to_string(), "Uno".to_string())]);
            assert!(!orq.has_pending_reaction(PRINCIPAL));
        }

        #[tokio::test]
        async fn non_delegated_runs_do_not_wake_the_orchestrator() {
            let (state, _orq, calls, _s, _w) = reaction_setup(ReactionMode::ReplyOnly);
            let rec = state
                .submit_task(&crate::CreateTaskRequest {
                    titulo: "Tarea suelta".into(),
                    prompt: "p".into(),
                    privacidad: Privacy::Open,
                    agent: AgentKind::Mock,
                    modelo: None,
                    esfuerzo: None,
                })
                .await
                .unwrap();
            state.scheduler.wait_run(&rec.id).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            assert!(
                calls.lock().unwrap().is_empty(),
                "solo las ejecuciones delegadas despiertan al orquestador"
            );
        }

        #[tokio::test]
        async fn wait_idle_times_out_while_busy_and_wakes_when_free() {
            struct SlowFake;
            impl ChatBackend for SlowFake {
                fn chat(&self, _prompt: String, _resume: Option<String>) -> ChatFuture<'_> {
                    Box::pin(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        Ok(ChatReply {
                            text: "listo".into(),
                            session_id: None,
                        })
                    })
                }
            }

            let orq = Orchestrator::new(Arc::new(SlowFake));
            assert!(
                !orq.wait_idle("no-existe", std::time::Duration::from_millis(5))
                    .await
            );

            let o = orq.clone();
            let turn =
                tokio::spawn(
                    async move { o.say_in_with(PRINCIPAL, "hola", 7777, None, None).await },
                );
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            while lock(&orq.inner)
                .map
                .get(PRINCIPAL)
                .unwrap()
                .in_flight
                .is_none()
            {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "el turno no llegó a empezar"
                );
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            assert!(
                !orq.wait_idle(PRINCIPAL, std::time::Duration::from_millis(20))
                    .await,
                "mientras hay un turno en curso debe agotarse el plazo"
            );
            assert!(
                orq.wait_idle(PRINCIPAL, std::time::Duration::from_secs(2))
                    .await,
                "al liberarse la conversación debe despertar sin esperar el plazo"
            );
            turn.await.unwrap().unwrap();
        }

        #[tokio::test]
        async fn valid_block_enqueues_mock_task_hides_block_and_feeds_next_turn() {
            let (state, orq, _s, _w, _h) = setup();
            let r1 = say(
                &state,
                &orq,
                "hazlo\nEMIT: [[forja:delegar {\"titulo\":\"Tarea uno\",\"prompt\":\"haz algo\",\"agent\":\"mock\",\"privacidad\":\"open\"}]]",
            )
            .await;
            assert_eq!(r1, "Delegando.", "los bloques se ocultan al usuario");
            let runs = state.scheduler.list_runs().await;
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].agent, AgentKind::Mock);
            assert_eq!(runs[0].task.titulo, "Tarea uno");
            let id = runs[0].id.clone();
            state.scheduler.wait_run(&id).await.unwrap();
            // El historial guarda el texto sin bloques.
            let h = orq.get(PRINCIPAL).unwrap().historial;
            assert!(h
                .iter()
                .all(|m| !m.texto.contains("[[forja:delegar") || m.rol == "usuario"));
            assert!(h
                .iter()
                .any(|m| m.rol == "sistema" && m.texto.contains(&id)));

            let r2 = say(&state, &orq, "¿cómo va?").await;
            assert!(r2.contains("Tareas delegadas (acciones"), "{r2}");
            assert!(r2.contains(&format!("ID {id}")), "{r2}");
            assert!(
                r2.contains("Tarea uno") && r2.contains("estado: done"),
                "{r2}"
            );
            // Se consume: el turno siguiente ya no lo repite.
            let r3 = say(&state, &orq, "gracias").await;
            assert!(!r3.contains(&format!("ID {id}")), "{r3}");
            assert!(
                !r3.contains("curl -s") && !r3.contains("http://127.0.0.1"),
                "sin curl en el prompt de acciones"
            );
        }

        #[tokio::test]
        async fn invalid_json_and_rejected_blocks_warn_next_turn_without_enqueueing() {
            let (state, orq, _s, _w, _h) = setup();
            let r1 = say(
                &state,
                &orq,
                "x\nEMIT: [[forja:delegar {roto]]\nEMIT: [[forja:delegar {\"titulo\":\"S\",\"prompt\":\"p\",\"agent\":\"mock\",\"privacidad\":\"sensitive\"}]]\nEMIT: [[forja:delegar {\"titulo\":\"S\",\"prompt\":\"p\",\"agent\":\"mock\",\"esfuerzo\":\"high\"}]]\nEMIT: [[forja:borrar]]",
            )
            .await;
            assert_eq!(r1, "Delegando.");
            assert!(state.scheduler.list_runs().await.is_empty());
            let r2 = say(&state, &orq, "¿y?").await;
            assert!(r2.contains("Aviso: delegar: JSON inválido"), "{r2}");
            assert!(
                r2.contains("rechazada"),
                "política de privacidad / esfuerzo: {r2}"
            );
            assert!(r2.contains("no admite esfuerzo"), "{r2}");
            assert!(r2.contains("acción desconocida"), "{r2}");
        }

        #[tokio::test]
        async fn limit_of_five_delegations_per_turn_is_enforced() {
            let (state, orq, _s, _w, _h) = setup();
            let line =
                "EMIT: [[forja:delegar {\"titulo\":\"T\",\"prompt\":\"p\",\"agent\":\"mock\"}]]";
            let msg = [line; 7].join("\n");
            say(&state, &orq, &msg).await;
            assert_eq!(
                state.scheduler.list_runs().await.len(),
                crate::delegation::MAX_DELEGATIONS_PER_TURN
            );
            let r2 = say(&state, &orq, "estado").await;
            assert!(r2.contains("se ignoraron 2 acción"), "{r2}");
        }

        #[tokio::test]
        async fn status_block_reports_a_run_and_unknown_ids() {
            let (state, orq, _s, _w, _h) = setup();
            let rec = state
                .submit_task(&crate::CreateTaskRequest {
                    titulo: "Previa".into(),
                    prompt: "p".into(),
                    privacidad: Privacy::Open,
                    agent: AgentKind::Mock,
                    modelo: None,
                    esfuerzo: None,
                })
                .await
                .unwrap();
            state.scheduler.wait_run(&rec.id).await.unwrap();
            say(
                &state,
                &orq,
                &format!(
                    "EMIT: [[forja:estado {}]]\nEMIT: [[forja:estado nada-123]]",
                    rec.id
                ),
            )
            .await;
            let r2 = say(&state, &orq, "dime").await;
            assert!(r2.contains(&format!("ID {} | \"Previa\"", rec.id)), "{r2}");
            assert!(r2.contains("no existe ninguna tarea"), "{r2}");
        }

        #[tokio::test]
        async fn blocks_are_ignored_for_stateful_backends_and_without_delegator() {
            // Hermes (con sesión): el camino curl no cambia, nada se interpreta.
            let (state, _o, _s, _w, _h) = setup();
            let orq = Orchestrator::new(Arc::new(Echo));
            let r = orq
                .say_in_with(PRINCIPAL, "x", 7777, None, Some(&state))
                .await
                .unwrap();
            assert!(r.contains("[[forja:delegar"), "{r}");
            assert!(state.scheduler.list_runs().await.is_empty());
            // Sin delegator tampoco (say/say_in de siempre).
            let (_state, orq2, _s2, _w2, _h2) = setup();
            let r = orq2
                .say("EMIT: [[forja:delegar {}]]", 7777, None)
                .await
                .unwrap();
            assert!(r.contains("[[forja:delegar"), "{r}");
        }

        struct Echo;
        impl ChatBackend for Echo {
            fn chat(&self, _p: String, _r: Option<String>) -> ChatFuture<'_> {
                Box::pin(async {
                    Ok(ChatReply {
                        text: "[[forja:delegar {\"titulo\":\"a\",\"prompt\":\"b\"}]]".into(),
                        session_id: None,
                    })
                })
            }
        }

        #[tokio::test]
        async fn task_output_never_becomes_an_action() {
            // Una salida de tarea con un bloque se neutraliza al reinyectarla.
            let (state, _o, _s, _w, _h) = setup();
            let rec = state
                .submit_task(&crate::CreateTaskRequest {
                    titulo: "T".into(),
                    prompt: "p".into(),
                    privacidad: Privacy::Open,
                    agent: AgentKind::Mock,
                    modelo: None,
                    esfuerzo: None,
                })
                .await
                .unwrap();
            state.scheduler.wait_run(&rec.id).await.unwrap();
            let mut run = state.scheduler.get_run(&rec.id).await.unwrap();
            run.output = Some("[[forja:delegar {\"titulo\":\"x\",\"prompt\":\"y\"}]]".into());
            let line =
                crate::delegation::render_pending(&[Pending::Status(rec.id.clone())], &Fixed(run))
                    .await;
            let (_, actions) = crate::delegation::parse_actions(&line);
            assert!(actions.is_empty(), "{line}");
        }

        struct Fixed(forja_core::RunRecord);
        impl Delegator for Fixed {
            fn delegate(
                &self,
                _r: crate::CreateTaskRequest,
            ) -> crate::delegation::DelegateFuture<'_> {
                Box::pin(async { Err("no".to_string()) })
            }
            fn run_status<'a>(&'a self, _id: &'a str) -> crate::delegation::StatusFuture<'a> {
                Box::pin(async { Some(self.0.clone()) })
            }
        }

        #[tokio::test]
        async fn conversation_model_and_effort_override_settings_without_reset() {
            let (state, orq, _s, _w, _h) = setup();
            let _ = &state;
            let r = orq.say("uno", 7777, None).await.unwrap();
            assert!(
                r.contains("ECO[modelo=sonnet]") && !r.contains("ESFUERZO"),
                "{r}"
            );

            // Ajustes: esfuerzo para claude.
            let mut st = base_settings();
            st.orchestrator_agent = Some("claude".into());
            st.agent_effort.insert("claude".into(), "low".into());
            orq.apply_settings(&st).unwrap();
            let r = orq.say("dos", 7777, None).await.unwrap();
            assert!(r.contains("ESFUERZO[low]"), "{r}");

            // La conversación gana sobre los ajustes y no reinicia el historial.
            orq.update_options(PRINCIPAL, Some("opus"), Some("MAX"))
                .unwrap();
            let r = orq.say("tres", 7777, None).await.unwrap();
            assert!(r.contains("ECO[modelo=opus] ESFUERZO[max]"), "{r}");
            assert!(r.contains("Usuario: uno"), "historial conservado: {r}");
            assert_eq!(
                orq.options_of(PRINCIPAL),
                Some((Some("opus".into()), Some("max".into())))
            );

            // Valores inválidos para la familia o conversación inexistente.
            assert!(matches!(
                orq.update_options(PRINCIPAL, None, Some("ultra")),
                Err(ChatError::Invalid(_))
            ));
            assert!(matches!(
                orq.update_options(PRINCIPAL, Some("--x"), None),
                Err(ChatError::Invalid(_))
            ));
            assert!(matches!(
                orq.update_options("nope", Some("a"), None),
                Err(ChatError::NotFound)
            ));
            // "" vuelve a los ajustes.
            orq.update_options(PRINCIPAL, Some(""), Some("")).unwrap();
            let r = orq.say("cuatro", 7777, None).await.unwrap();
            assert!(r.contains("ECO[modelo=sonnet] ESFUERZO[low]"), "{r}");
        }

        #[tokio::test]
        async fn opencode_effort_becomes_model_variant_and_local_rejects_it() {
            let oc = CliBackend::new("opencode", BTreeMap::new()).with_program(fake_script());
            let opts = ChatOpts {
                model: Some("p/m#low".into()),
                effort: Some("High".into()),
            };
            let r = oc.chat_with("hola".into(), None, &opts).await.unwrap();
            assert!(r.text.contains("ECO[modelo=p/m#high]"), "{}", r.text);
            let r = oc
                .chat_with(
                    "hola".into(),
                    None,
                    &ChatOpts {
                        model: Some("p/m#low".into()),
                        effort: None,
                    },
                )
                .await
                .unwrap();
            assert!(r.text.contains("ECO[modelo=p/m#low]"), "{}", r.text);
            let local = CliBackend::new("local", BTreeMap::new()).with_program(fake_script());
            let err = local
                .chat_with(
                    "hola".into(),
                    None,
                    &ChatOpts {
                        model: None,
                        effort: Some("high".into()),
                    },
                )
                .await
                .unwrap_err();
            assert!(err.contains("no admite esfuerzo"), "{err}");
            // agy usa --effort.
            let agy = CliBackend::new("agy", BTreeMap::new()).with_program(fake_script());
            let r = agy
                .chat_with(
                    "hola".into(),
                    None,
                    &ChatOpts {
                        model: None,
                        effort: Some("xhigh".into()),
                    },
                )
                .await
                .unwrap();
            assert!(r.text.contains("ESFUERZO[xhigh]"), "{}", r.text);
        }

        #[tokio::test]
        async fn hermes_orchestrator_passes_model_and_reasoning() {
            use std::os::unix::fs::PermissionsExt;
            let dir = TempDir::new().unwrap();
            let script = dir.path().join("fake_hermes_args.sh");
            std::fs::write(&script, "#!/usr/bin/env bash\necho \"ARGS: $*\"\n").unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let (_home, rules) = fake_rules(&[]);
            let b = HermesBackend::new(rules).with_program(script.to_string_lossy());
            let r = b.chat("hola".into(), None).await.unwrap();
            assert!(
                !r.text.contains(" -m ") && !r.text.contains("--reasoning"),
                "{}",
                r.text
            );
            // Ajustes: modelo y esfuerzo de hermes.
            b.set_models(&[("hermes".to_string(), "gpt-x".to_string())].into());
            b.set_efforts(&[("hermes".to_string(), "high".to_string())].into());
            let r = b.chat("hola".into(), None).await.unwrap();
            assert!(r.text.contains("-m gpt-x --reasoning high"), "{}", r.text);
            // La conversación gana.
            let opts = ChatOpts {
                model: Some("otro".into()),
                effort: Some("ULTRA".into()),
            };
            let r = b
                .chat_with("hola".into(), Some("sess".into()), &opts)
                .await
                .unwrap();
            assert!(
                r.text.contains("--resume sess -m otro --reasoning ultra"),
                "{}",
                r.text
            );
            let bad = ChatOpts {
                model: None,
                effort: Some("extremo".into()),
            };
            assert!(b.chat_with("hola".into(), None, &bad).await.is_err());
        }

        #[tokio::test]
        async fn chat_body_modelo_and_esfuerzo_are_stored_per_conversation() {
            use axum::body::{to_bytes, Body};
            use axum::http::{header, Method, Request};
            use tower::ServiceExt;
            let (state, orq, _s, _w, _h) = setup();
            let app = crate::build_router(state);
            let chat = |body: Value| {
                let app = app.clone();
                async move {
                    let req = Request::builder()
                        .header(header::HOST, "127.0.0.1:7777")
                        .method(Method::POST)
                        .uri("/api/orchestrator/chat")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap();
                    let resp = app.oneshot(req).await.unwrap();
                    let st = resp.status();
                    let b = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                    (
                        st,
                        serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null),
                    )
                }
            };
            let (st, v) =
                chat(json!({"message": "hola", "modelo": "haiku", "esfuerzo": "medium"})).await;
            assert_eq!(st, StatusCode::OK, "{v}");
            let reply = v["reply"].as_str().unwrap();
            assert!(
                reply.contains("ECO[modelo=haiku] ESFUERZO[medium]"),
                "{reply}"
            );
            let d = orq.get(PRINCIPAL).unwrap().resumen;
            assert_eq!(d.modelo.as_deref(), Some("haiku"));
            assert_eq!(d.esfuerzo.as_deref(), Some("medium"));
            // Sin campos en el cuerpo se conservan; inválido -> 400 sin llamar al agente.
            let (_, v) = chat(json!({"message": "otra"})).await;
            assert!(v["reply"].as_str().unwrap().contains("ESFUERZO[medium]"));
            let (st, _) = chat(json!({"message": "x", "esfuerzo": "ultra"})).await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
            let (st, _) =
                chat(json!({"message": "x", "conversation_id": "nope", "modelo": "a"})).await;
            assert_eq!(st, StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn slash_model_and_effort_commands_over_http() {
            use axum::body::{to_bytes, Body};
            use axum::http::{header, Method, Request};
            use tower::ServiceExt;
            let (state, orq, _s, _w, _h) = setup();
            let app = crate::build_router(state);
            let call = |msg: &'static str| {
                let app = app.clone();
                async move {
                    let req = Request::builder()
                        .header(header::HOST, "127.0.0.1:7777")
                        .method(Method::POST)
                        .uri("/api/orchestrator/chat")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(json!({ "message": msg }).to_string()))
                        .unwrap();
                    let resp = app.oneshot(req).await.unwrap();
                    let st = resp.status();
                    let b = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                    (
                        st,
                        serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null),
                    )
                }
            };
            let (st, v) = call("/model opus").await;
            assert_eq!(st, StatusCode::OK, "{v}");
            assert!(v["reply"].as_str().unwrap().contains("cambiado a opus"));
            let (st, v) = call("/effort high").await;
            assert_eq!(st, StatusCode::OK, "{v}");
            assert_eq!(
                orq.options_of(PRINCIPAL),
                Some((Some("opus".into()), Some("high".into())))
            );
            let (_, v) = call("/effort").await;
            assert!(v["reply"].as_str().unwrap().contains("high"));
            let (st, _) = call("/effort ultra").await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
            let (st, _) = call("/model reset").await;
            assert_eq!(st, StatusCode::OK);
            assert_eq!(orq.options_of(PRINCIPAL).unwrap().0, None);
            // Los comandos no tocan el historial.
            assert_eq!(orq.get(PRINCIPAL).unwrap().historial.len(), 0);
        }
    }

    mod http {
        use super::*;
        use axum::body::{to_bytes, Body};
        use axum::http::{header, Method, Request};
        use forja_sched::{Scheduler, SchedulerConfig};
        use forja_store::JsonlStore;
        use tempfile::TempDir;
        use tower::ServiceExt;

        fn app(orq: Orchestrator) -> (axum::Router, TempDir, TempDir) {
            let s = TempDir::new().unwrap();
            let w = TempDir::new().unwrap();
            let store = JsonlStore::new(s.path()).unwrap();
            let sched = Scheduler::new(store, SchedulerConfig::new(w.path())).unwrap();
            let state = AppState::new(sched).with_orchestrator(orq);
            (crate::build_router(state), s, w)
        }

        async fn call(
            app: &axum::Router,
            method: Method,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut b = Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(method)
                .uri(uri);
            let body = match body {
                Some(v) => {
                    b = b.header(header::CONTENT_TYPE, "application/json");
                    Body::from(v.to_string())
                }
                None => Body::empty(),
            };
            let resp = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
            let status = resp.status();
            let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, v)
        }

        #[tokio::test]
        async fn chat_without_conversation_id_uses_principal_and_reset_works() {
            let orq = Orchestrator::new(SessionFake::new());
            let (app, _s, _w) = app(orq.clone());
            let (st, v) = call(
                &app,
                Method::POST,
                "/api/orchestrator/chat",
                Some(json!({"message": "hola"})),
            )
            .await;
            assert_eq!(st, StatusCode::OK);
            assert_eq!(v["reply"], "ok");
            assert_eq!(v["conversation_id"], PRINCIPAL);
            assert!(orq.current_session().is_some());
            let (st, _) = call(&app, Method::POST, "/api/orchestrator/reset", None).await;
            assert_eq!(st, StatusCode::NO_CONTENT);
            assert!(orq.current_session().is_none());
        }

        #[tokio::test]
        async fn crud_endpoints_and_conversation_id_in_chat() {
            let (app, _s, _w) = app(Orchestrator::new(SessionFake::new()));
            let (st, v) = call(
                &app,
                Method::POST,
                "/api/orchestrator/conversations",
                Some(json!({"title": "Uno"})),
            )
            .await;
            assert_eq!(st, StatusCode::CREATED);
            let id = v["id"].as_str().unwrap().to_string();
            let (_, v) = call(
                &app,
                Method::POST,
                "/api/orchestrator/chat",
                Some(json!({"message": "hola", "conversation_id": id})),
            )
            .await;
            assert_eq!(v["conversation_id"], id.as_str());
            let (st, v) = call(
                &app,
                Method::GET,
                &format!("/api/orchestrator/conversations/{id}"),
                None,
            )
            .await;
            assert_eq!(st, StatusCode::OK);
            assert_eq!(v["titulo"], "Uno");
            assert_eq!(v["historial"].as_array().unwrap().len(), 2);
            let (_, v) = call(&app, Method::GET, "/api/orchestrator/conversations", None).await;
            assert_eq!(v.as_array().unwrap().len(), 2);
            // reset por conversación
            let (st, _) = call(
                &app,
                Method::POST,
                "/api/orchestrator/reset",
                Some(json!({"conversation_id": id})),
            )
            .await;
            assert_eq!(st, StatusCode::NO_CONTENT);
            let (st, _) = call(
                &app,
                Method::DELETE,
                &format!("/api/orchestrator/conversations/{id}"),
                None,
            )
            .await;
            assert_eq!(st, StatusCode::NO_CONTENT);
            let (st, _) = call(
                &app,
                Method::GET,
                &format!("/api/orchestrator/conversations/{id}"),
                None,
            )
            .await;
            assert_eq!(st, StatusCode::NOT_FOUND);
            let (st, _) = call(
                &app,
                Method::POST,
                "/api/orchestrator/chat",
                Some(json!({"message": "hola", "conversation_id": id})),
            )
            .await;
            assert_eq!(st, StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn slash_commands_over_http() {
            let orq = Orchestrator::new(SessionFake::new());
            let (app, _s, _w) = app(orq.clone());
            let chat = |msg: &'static str, id: Option<&'static str>| {
                let app = app.clone();
                async move {
                    call(
                        &app,
                        Method::POST,
                        "/api/orchestrator/chat",
                        Some(json!({"message": msg, "conversation_id": id})),
                    )
                    .await
                }
            };
            let (st, v) = chat("/new", None).await;
            assert_eq!(st, StatusCode::OK);
            assert_eq!(v["new_conversation"], true);
            let new_id = v["conversation_id"].as_str().unwrap().to_string();
            assert_ne!(new_id, PRINCIPAL);
            assert!(orq.get(&new_id).is_some());

            let (_, v) = chat("/help", None).await;
            let r = v["reply"].as_str().unwrap();
            assert!(r.contains("/rename") && r.contains("/stop") && r.contains("/agents"));
            assert!(r.contains("/model") && r.contains("/effort"), "{r}");

            let (st, _) = chat("/rename", None).await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
            let (st, v) = chat("/rename Nuevo nombre", None).await;
            assert_eq!(st, StatusCode::OK);
            assert_eq!(v["conversation_id"], PRINCIPAL);
            assert_eq!(orq.get(PRINCIPAL).unwrap().resumen.titulo, "Nuevo nombre");

            let (st, v) = chat("/agents", None).await;
            assert_eq!(st, StatusCode::OK);
            assert!(v["reply"].as_str().unwrap().contains("mock"));
            assert!(!v["agents"].as_array().unwrap().is_empty());

            let (st, v) = chat("/stop", None).await;
            assert_eq!(st, StatusCode::OK);
            assert_eq!(v["stopped"], false);
            assert!(v["reply"].as_str().unwrap().contains("nada en curso"));

            let (st, v) = chat("/agent claude", None).await;
            assert_eq!(st, StatusCode::OK);
            assert!(v["reply"].as_str().unwrap().contains("pronto"));
            // /model y /effort sin argumento solo muestran el valor.
            for cmd in ["/model", "/effort"] {
                let (st, v) = chat(cmd, None).await;
                assert_eq!(st, StatusCode::OK);
                assert!(v["reply"].as_str().unwrap().contains("Ajustes"), "{v}");
            }
            // El doble de pruebas no es una familia conocida: no admite elegir.
            let (st, _) = chat("/model x", None).await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
            let (st, _) = chat("/loquesea", None).await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
            // los comandos no entran en el historial ni llaman al backend
            assert_eq!(orq.get(PRINCIPAL).unwrap().historial.len(), 0);
        }

        #[tokio::test]
        async fn new_command_reports_limit() {
            let orq = Orchestrator::new(SessionFake::new());
            for _ in 1..MAX_CONVERSATIONS {
                orq.create(None).unwrap();
            }
            let (app, _s, _w) = app(orq);
            let (st, _) = call(
                &app,
                Method::POST,
                "/api/orchestrator/chat",
                Some(json!({"message": "/new"})),
            )
            .await;
            assert_eq!(st, StatusCode::CONFLICT);
        }
    }
}
