//! Flujo de chat del orquestador: construcción del prompt del sistema, turnos
//! del usuario y automáticos, y handler HTTP `/api/orchestrator/chat`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::delegation::{self, Delegator, Pending};
use crate::AppState;

use super::backends::ChatOpts;
use super::commands::{parse_command, run_command};
use super::conversations::{
    auto_title, conv_id, history_block, lock, not_found, ChatError, InFlight, InFlightGuard,
    PRINCIPAL,
};
use super::Orchestrator;

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

impl Orchestrator {
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
        self.persist();
        // El servidor vigila estas ejecuciones y despierta al orquestador al acabar.
        if let Some(d) = delegator {
            for run_id in delegated_runs {
                d.track_delegation(id, &run_id);
            }
        }
        result
    }
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
