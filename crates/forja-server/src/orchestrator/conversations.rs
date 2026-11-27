//! Gestión de conversaciones del orquestador: estado en memoria, historial,
//! persistencia, opciones por conversación y endpoints CRUD/reset.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use forja_core::AgentKind;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::conversation_store::{
    load_conversations, ConversationPersistence, PersistedConversation,
};
use crate::delegation::Pending;
use crate::AppState;

use super::Orchestrator;

/// Intercambios (usuario + respuesta) que se reenvían a los CLIs sin sesión.
pub const HISTORY_EXCHANGES: usize = 20;
const HISTORY_MSG_CHARS: usize = 1500;
const HISTORY_TOTAL_CHARS: usize = 30_000;

/// Id de la conversación por defecto (la que usan `/chat` y `/reset` sin `conversation_id`).
pub const PRINCIPAL: &str = "principal";
/// Máximo de conversaciones vivas (incluida la principal).
pub const MAX_CONVERSATIONS: usize = 20;
/// Máximo de mensajes por conversación; al pasarse se descartan los más antiguos.
pub const MAX_MESSAGES: usize = 500;
const MAX_TITLE_CHARS: usize = 60;
const AUTO_TITLE_WORDS: usize = 6;

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

pub(super) struct InFlight {
    pub(super) seq: u64,
    pub(super) cancel: Arc<Notify>,
    /// `true` si es un turno automático (el usuario tiene prioridad).
    pub(super) auto: bool,
}

pub(super) struct Conversation {
    pub(super) title: String,
    pub(super) session_id: Option<String>,
    pub(super) messages: Vec<Message>,
    pub(super) created: u64,
    pub(super) updated: u64,
    pub(super) in_flight: Option<InFlight>,
    /// Modelo y esfuerzo propios de la conversación (sobrescriben los ajustes).
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
    /// Acciones delegadas del último turno, a reinyectar en el siguiente.
    pub(super) pending: Vec<Pending>,
    /// Turnos automáticos seguidos desde el último mensaje del usuario.
    pub(super) auto_turns: usize,
}

impl Conversation {
    pub(super) fn new(title: &str) -> Self {
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

    pub(super) fn push(&mut self, rol: &str, texto: &str) {
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

    pub(super) fn summary(&self, id: &str) -> ConversationSummary {
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

pub(super) struct Inner {
    pub(super) map: HashMap<String, Conversation>,
    pub(super) counter: u64,
    pub(super) seq: u64,
    /// Se notifica cuando una conversación deja de estar ocupada.
    pub(super) idle: Arc<Notify>,
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

/// Título automático: primeras palabras del primer mensaje.
pub fn auto_title(message: &str) -> String {
    let words: Vec<&str> = message.split_whitespace().take(AUTO_TITLE_WORDS).collect();
    clamp_title(&words.join(" "))
}

pub(super) fn clamp_title(title: &str) -> String {
    title.trim().chars().take(MAX_TITLE_CHARS).collect()
}

fn parse_conv_counter(id: &str) -> Option<u64> {
    let rest = id.strip_prefix('c')?;
    let (num, _) = rest.split_once('-')?;
    num.parse().ok()
}

/// Libera la marca "en curso" aunque la petición HTTP se aborte a mitad.
pub(super) struct InFlightGuard {
    pub(super) inner: Arc<Mutex<Inner>>,
    pub(super) id: String,
    pub(super) seq: u64,
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

pub(super) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Orchestrator {
    /// Activa la persistencia en `path` (`<store_dir>/conversations.json`), cargando las
    /// conversaciones válidas existentes sin impedir el arranque si falta o está corrupto.
    pub fn with_persistence(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let loaded = load_conversations(&path);
        if !loaded.is_empty() {
            let mut g = lock(&self.inner);
            let mut map = HashMap::with_capacity(loaded.len() + 1);
            let mut max_counter = g.counter;
            for item in loaded {
                if let Some(n) = parse_conv_counter(&item.id) {
                    max_counter = max_counter.max(n);
                }
                map.insert(
                    item.id,
                    Conversation {
                        title: item.titulo,
                        session_id: item.session_id,
                        messages: item.historial,
                        created: item.creado,
                        updated: item.actualizado,
                        in_flight: None,
                        model: item.modelo,
                        effort: item.esfuerzo,
                        pending: Vec::new(),
                        auto_turns: 0,
                    },
                );
            }
            map.entry(PRINCIPAL.to_string())
                .or_insert_with(|| Conversation::new("Principal"));
            g.map = map;
            g.counter = max_counter;
        }
        self.persistence = Some(Arc::new(ConversationPersistence::new(path)));
        self
    }

    /// Espera a que termine cualquier escritura pendiente en disco (útil en tests y apagado).
    pub fn flush_persistence(&self) {
        if let Some(p) = &self.persistence {
            p.flush();
        }
    }

    /// Programa un guardado coalescido fuera del lock de `Inner`.
    pub(super) fn persist(&self) {
        let Some(p) = &self.persistence else {
            return;
        };
        let snapshot: Vec<PersistedConversation> = {
            let g = lock(&self.inner);
            g.map
                .iter()
                .map(|(id, c)| PersistedConversation {
                    id: id.clone(),
                    titulo: c.title.clone(),
                    session_id: c.session_id.clone(),
                    historial: c.messages.clone(),
                    creado: c.created,
                    actualizado: c.updated,
                    modelo: c.model.clone(),
                    esfuerzo: c.effort.clone(),
                })
                .collect()
        };
        p.schedule(snapshot);
    }

    pub(super) fn clear_conversation_options(&self) {
        let mut changed = false;
        {
            let mut g = lock(&self.inner);
            for c in g.map.values_mut() {
                if c.model.take().is_some() {
                    changed = true;
                }
                if c.effort.take().is_some() {
                    changed = true;
                }
            }
        }
        if changed {
            self.persist();
        }
    }

    /// Sesión de Hermes de la conversación principal.
    pub fn current_session(&self) -> Option<String> {
        self.session_of(PRINCIPAL)
    }

    /// Olvida la sesión de Hermes de todas las conversaciones (cambio de perfil o agente).
    pub(super) fn reset_all_sessions(&self) {
        let mut changed = false;
        {
            let mut g = lock(&self.inner);
            for c in g.map.values_mut() {
                if let Some(f) = c.in_flight.take() {
                    f.cancel.notify_one();
                }
                if c.session_id.take().is_some() {
                    changed = true;
                }
                c.auto_turns = 0;
            }
        }
        if changed {
            self.persist();
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
        let summary = {
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
            summary
        };
        self.persist();
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
            self.persist();
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
            self.persist();
        }
        existed
    }

    /// Reinicia la conversación principal.
    pub fn reset(&self) {
        self.reset_conversation(PRINCIPAL);
    }

    pub fn rename(&self, id: &str, title: &str) -> bool {
        let renamed = {
            let mut g = lock(&self.inner);
            match g.map.get_mut(id) {
                Some(c) => {
                    c.title = clamp_title(title);
                    c.updated = now_ms();
                    true
                }
                None => false,
            }
        };
        if renamed {
            self.persist();
        }
        renamed
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

    /// Añade una nota del sistema al historial (errores del turno automático).
    pub fn push_system(&self, conversation: &str, text: &str) {
        let pushed = {
            let mut g = lock(&self.inner);
            if let Some(c) = g.map.get_mut(conversation) {
                c.push("sistema", text);
                true
            } else {
                false
            }
        };
        if pushed {
            self.persist();
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
        let changed = {
            let mut g = lock(&self.inner);
            let conv = g.map.get_mut(id).ok_or(ChatError::NotFound)?;
            let mut changed = false;
            if let Some(m) = model {
                if conv.model != m {
                    conv.model = m;
                    changed = true;
                }
            }
            if let Some(e) = effort {
                if conv.effort != e {
                    conv.effort = e;
                    changed = true;
                }
            }
            changed
        };
        if changed {
            self.persist();
        }
        Ok(())
    }
}

pub(super) fn conv_id(raw: &Option<String>) -> String {
    raw.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(PRINCIPAL)
        .to_string()
}

pub(super) fn not_found() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "conversación no encontrada" })),
    )
}

pub(super) fn limit_reached() -> (StatusCode, Json<Value>) {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": format!("límite de {MAX_CONVERSATIONS} conversaciones; borra alguna")
        })),
    )
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
