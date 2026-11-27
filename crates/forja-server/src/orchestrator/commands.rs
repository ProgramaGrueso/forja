//! Comandos de barra (`/new`, `/help`, `/rename`, `/agents`, `/stop`, `/model`,
//! `/effort`, `/agent`) resueltos localmente por el servidor.

use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::AppState;

use super::conversations::{clamp_title, limit_reached, not_found, ChatError, CreateError};

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

pub(super) async fn run_command(
    state: &AppState,
    id: &str,
    cmd: Command,
) -> (StatusCode, Json<Value>) {
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
