//! Orquestador conversacional: un agente de Hermes que habla con el usuario,
//! delega tareas en Forja y revisa sus resultados. El panel lo usa por texto o
//! por voz (Web Speech API en el navegador).

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use forja_agent::hermes::validate_profile;
use forja_agent::Runner;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::AppState;

/// Variable de entorno con el perfil de Hermes que hace de orquestador.
pub const PROFILE_ENV: &str = "FORJA_ORCHESTRATOR_PROFILE";
const TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, PartialEq)]
pub struct ChatReply {
    pub text: String,
    pub session_id: Option<String>,
}

pub type ChatFuture<'a> = Pin<Box<dyn Future<Output = Result<ChatReply, String>> + Send + 'a>>;

/// Motor de conversación; se sustituye por un doble en las pruebas.
pub trait ChatBackend: Send + Sync {
    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_>;

    /// Perfil activo (None si el backend no tiene perfiles).
    fn profile(&self) -> Option<String> {
        None
    }

    /// Cambia el perfil en caliente (no-op si el backend no tiene perfiles).
    fn set_profile(&self, _profile: &str) {}
}

/// Backend real: `hermes -p <perfil> chat --query-file <f> -Q --oneshot [--resume <id>]`.
pub struct HermesBackend {
    profile: RwLock<String>,
}

impl HermesBackend {
    pub fn from_env() -> Self {
        Self {
            profile: RwLock::new(resolve_profile(None, env_profile().as_deref())),
        }
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

    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
        Box::pin(async move {
            let wanted = self.profile().unwrap_or_default();
            let profile = validate_profile(&wanted).map_err(|e| e.to_string())?;
            let dir = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let file = dir.join(format!("forja-orq-{}-{nanos}.txt", std::process::id()));
            std::fs::write(&file, &prompt)
                .map_err(|e| format!("no se pudo escribir la consulta: {e}"))?;
            let argv = build_argv(&profile, &file, resume.as_deref());
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

/// Instrucciones del primer turno. La respuesta se lee en voz alta, así que debe ser hablada.
pub fn system_prompt(port: u16, token: Option<&str>) -> String {
    let auth = token
        .map(|t| format!(" -H 'Authorization: Bearer {t}'"))
        .unwrap_or_default();
    format!(
        "Eres el orquestador de Forja. Tu función es delegar y revisar, no hacer el trabajo tú.\n\
         Reglas:\n\
         - SIEMPRE respondes al usuario, aunque sea para decir qué estás haciendo.\n\
         - Tu respuesta se lee en voz alta: español natural, frases cortas, sin markdown, sin listas, sin código.\n\
         - Para delegar usa la API local de Forja con curl:\n\
           curl -s -X POST http://127.0.0.1:{port}/tasks -H 'Content-Type: application/json'{auth} -d '{{\"titulo\":\"...\",\"prompt\":\"...\",\"privacidad\":\"open\",\"agent\":\"opencode\"}}'\n\
           agentes: claude, opencode, agy, hermes, local, mock. Privacidad: open o sensitive (sensitive solo admite claude).\n\
         - Revisa el avance con curl http://127.0.0.1:{port}/runs/ID{auth} (estado y salida) y revisa el resultado antes de decir que está listo.\n\
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
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::NotFound => write!(f, "conversación no encontrada"),
            ChatError::Busy => write!(f, "la conversación ya tiene una llamada en curso"),
            ChatError::Cancelled => write!(f, "llamada cancelada"),
            ChatError::Backend(e) => write!(f, "{e}"),
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
}

struct Conversation {
    title: String,
    session_id: Option<String>,
    messages: Vec<Message>,
    created: u64,
    updated: u64,
    in_flight: Option<InFlight>,
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
        }
    }
}

struct Inner {
    map: HashMap<String, Conversation>,
    counter: u64,
    seq: u64,
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
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
pub struct Orchestrator {
    backend: Arc<dyn ChatBackend>,
    inner: Arc<Mutex<Inner>>,
}

impl Orchestrator {
    pub fn new(backend: Arc<dyn ChatBackend>) -> Self {
        let mut map = HashMap::new();
        map.insert(PRINCIPAL.to_string(), Conversation::new("Principal"));
        Self {
            backend,
            inner: Arc::new(Mutex::new(Inner {
                map,
                counter: 0,
                seq: 0,
            })),
        }
    }

    /// Único punto donde se decide el backend (y con él el perfil de Hermes).
    pub fn from_env() -> Self {
        Self::new(Arc::new(HermesBackend::from_env()))
    }

    /// Sesión de Hermes de la conversación principal.
    pub fn current_session(&self) -> Option<String> {
        self.session_of(PRINCIPAL)
    }

    /// Aplica el perfil de los ajustes (con env y `default` como respaldo). Si el
    /// perfil cambia, se reinicia la sesión. Devuelve el perfil efectivo.
    pub fn apply_profile(&self, setting: Option<&str>) -> Result<String, String> {
        let wanted = resolve_profile(setting, env_profile().as_deref());
        let profile = validate_profile(&wanted).map_err(|e| e.to_string())?;
        if self.backend.profile().as_deref() != Some(profile.as_str()) {
            self.backend.set_profile(&profile);
            self.reset_all_sessions();
        }
        Ok(profile)
    }

    pub fn profile(&self) -> Option<String> {
        self.backend.profile()
    }

    /// Olvida la sesión de Hermes de todas las conversaciones (cambio de perfil).
    fn reset_all_sessions(&self) {
        let mut g = lock(&self.inner);
        for c in g.map.values_mut() {
            if let Some(f) = c.in_flight.take() {
                f.cancel.notify_one();
            }
            c.session_id = None;
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
        let mut g = lock(&self.inner);
        if id == PRINCIPAL {
            if let Some(c) = g.map.get_mut(PRINCIPAL) {
                if let Some(f) = c.in_flight.take() {
                    f.cancel.notify_one();
                }
                *c = Conversation::new("Principal");
            }
            return true;
        }
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

    /// Olvida la sesión de Hermes y el historial, conservando id y título.
    pub fn reset_conversation(&self, id: &str) -> bool {
        let mut g = lock(&self.inner);
        match g.map.get_mut(id) {
            Some(c) => {
                if let Some(f) = c.in_flight.take() {
                    f.cancel.notify_one();
                }
                c.session_id = None;
                c.messages.clear();
                c.updated = now_ms();
                true
            }
            None => false,
        }
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
        let (resume, cancel, seq) = {
            let mut g = lock(&self.inner);
            g.seq += 1;
            let seq = g.seq;
            let conv = g.map.get_mut(id).ok_or(ChatError::NotFound)?;
            if conv.in_flight.is_some() {
                return Err(ChatError::Busy);
            }
            if conv.title.is_empty() {
                conv.title = auto_title(message);
            }
            conv.push("usuario", message);
            let cancel = Arc::new(Notify::new());
            conv.in_flight = Some(InFlight {
                seq,
                cancel: cancel.clone(),
            });
            (conv.session_id.clone(), cancel, seq)
        };
        let _guard = InFlightGuard {
            inner: self.inner.clone(),
            id: id.to_string(),
            seq,
        };
        let prompt = if resume.is_some() {
            message.to_string()
        } else {
            format!("{}{message}", system_prompt(port, token))
        };
        let outcome = tokio::select! {
            r = self.backend.chat(prompt, resume) => Some(r),
            _ = cancel.notified() => None,
        };
        let mut g = lock(&self.inner);
        let Some(conv) = g.map.get_mut(id) else {
            // Borrada mientras hablaba: no hay dónde guardar nada.
            return match outcome {
                Some(Ok(reply)) => Ok(reply.text),
                Some(Err(e)) => Err(ChatError::Backend(e)),
                None => Err(ChatError::Cancelled),
            };
        };
        match outcome {
            Some(Ok(reply)) => {
                if let Some(sid) = reply.session_id {
                    conv.session_id = Some(sid);
                }
                conv.push("orquestador", &reply.text);
                Ok(reply.text)
            }
            Some(Err(e)) => Err(ChatError::Backend(e)),
            None => {
                conv.push("sistema", "Llamada cancelada con /stop.");
                Err(ChatError::Cancelled)
            }
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
    /// Reservado: lo implementa otro stream.
    Model(String),
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
    ("/model", "disponible pronto"),
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
        "/model" => Command::Model(arg),
        "/agent" => Command::Agent(arg),
        _ => Command::Unknown(word),
    })
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(default)]
    pub conversation_id: Option<String>,
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
        .say_in(&id, message, port, state.auth_token.as_deref())
        .await
    {
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
        Command::Model(_) | Command::Agent(_) => {
            ok(json!({ "reply": "Disponible pronto.", "conversation_id": id }))
        }
        Command::Unknown(cmd) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("comando desconocido: {cmd}. Usa /help para ver la lista.")
            })),
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

    #[tokio::test]
    async fn apply_profile_switches_hot_and_resets_session_only_on_change() {
        let backend = Arc::new(Profiled(RwLock::new("default".into())));
        let orq = Orchestrator::new(backend.clone());
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
    fn apply_profile_rejects_private_and_unknown_profiles() {
        let backend = Arc::new(Profiled(RwLock::new("default".into())));
        let orq = Orchestrator::new(backend.clone());
        for bad in ["intimo", "mama", "otro"] {
            assert!(orq.apply_profile(Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(backend.profile().as_deref(), Some("default"));
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
            Some(Command::Unknown("/modelo".into()))
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
            let mut b = Request::builder().method(method).uri(uri);
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

            for cmd in ["/model x", "/agent claude"] {
                let (st, v) = chat(cmd, None).await;
                assert_eq!(st, StatusCode::OK);
                assert!(v["reply"].as_str().unwrap().contains("pronto"));
            }
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
