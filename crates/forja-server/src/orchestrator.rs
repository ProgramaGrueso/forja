//! Orquestador conversacional: un agente de Hermes que habla con el usuario,
//! delega tareas en Forja y revisa sus resultados. El panel lo usa por texto o
//! por voz (Web Speech API en el navegador).

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use forja_agent::hermes::validate_profile;
use forja_agent::Runner;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
}

/// Backend real: `hermes -p <perfil> chat --query-file <f> -Q --oneshot [--resume <id>]`.
pub struct HermesBackend {
    profile: String,
}

impl HermesBackend {
    pub fn from_env() -> Self {
        let wanted = std::env::var(PROFILE_ENV).unwrap_or_else(|_| "default".to_string());
        Self { profile: wanted }
    }
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
    fn chat(&self, prompt: String, resume: Option<String>) -> ChatFuture<'_> {
        Box::pin(async move {
            let profile = validate_profile(&self.profile).map_err(|e| e.to_string())?;
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

#[derive(Clone)]
pub struct Orchestrator {
    backend: Arc<dyn ChatBackend>,
    session: Arc<Mutex<Option<String>>>,
}

impl Orchestrator {
    pub fn new(backend: Arc<dyn ChatBackend>) -> Self {
        Self {
            backend,
            session: Arc::new(Mutex::new(None)),
        }
    }

    pub fn from_env() -> Self {
        Self::new(Arc::new(HermesBackend::from_env()))
    }

    fn current_session(&self) -> Option<String> {
        self.session.lock().ok().and_then(|g| g.clone())
    }

    fn reset(&self) {
        if let Ok(mut g) = self.session.lock() {
            *g = None;
        }
    }

    pub async fn say(
        &self,
        message: &str,
        port: u16,
        token: Option<&str>,
    ) -> Result<String, String> {
        let resume = self.current_session();
        let prompt = if resume.is_some() {
            message.to_string()
        } else {
            format!("{}{message}", system_prompt(port, token))
        };
        let reply = self.backend.chat(prompt, resume).await?;
        if let (Some(id), Ok(mut g)) = (reply.session_id, self.session.lock()) {
            *g = Some(id);
        }
        Ok(reply.text)
    }
}

/// Comandos de barra que el panel resuelve sin llamar a Hermes.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    New,
    Help,
    Unknown(String),
}

/// Devuelve `None` si el mensaje no es un comando (va normal al orquestador).
pub fn parse_command(message: &str) -> Option<Command> {
    let trimmed = message.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let word = trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    Some(match word.as_str() {
        "/new" | "/reset" | "/nueva" => Command::New,
        "/help" | "/ayuda" | "/comandos" => Command::Help,
        _ => Command::Unknown(word),
    })
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
}

#[derive(Debug, Serialize)]
struct ChatResponse {
    reply: String,
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
    let port = state.settings.read().map(|s| s.port).unwrap_or(7777);
    if let Some(cmd) = parse_command(message) {
        return match cmd {
            Command::New => {
                state.orchestrator.reset();
                (
                    StatusCode::OK,
                    Json(json!({ "reply": "Sesión nueva. ¿Qué hacemos?" })),
                )
            }
            Command::Help => (
                StatusCode::OK,
                Json(json!({
                    "reply": "Comandos: /new para empezar una sesión nueva, /help para ver esto."
                })),
            ),
            Command::Unknown(cmd) => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!("comando desconocido: {cmd}. Usa /new o /help.")
                })),
            ),
        };
    }
    match state
        .orchestrator
        .say(message, port, state.auth_token.as_deref())
        .await
    {
        Ok(reply) => (StatusCode::OK, Json(json!(ChatResponse { reply }))),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": error }))),
    }
}

pub async fn reset(State(state): State<AppState>) -> StatusCode {
    state.orchestrator.reset();
    StatusCode::NO_CONTENT
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
}
