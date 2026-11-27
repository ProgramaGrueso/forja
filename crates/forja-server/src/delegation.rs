//! Delegación del orquestador sin permisos de shell.
//!
//! Los orquestadores que no son Hermes (claude, opencode, agy, local) no ejecutan
//! `curl`: terminan su respuesta con líneas de acción y el SERVIDOR las interpreta:
//!
//! ```text
//! [[forja:delegar {"titulo":"...","prompt":"...","agent":"opencode","privacidad":"open"}]]
//! [[forja:estado ID]]
//! ```
//!
//! Reglas de seguridad:
//! - Solo se parsea la salida del orquestador, nunca la de las tareas (la salida de
//!   las tareas se reinyecta neutralizando cualquier `[[forja:`).
//! - Una acción es una LÍNEA que empieza (tras espacios) por `[[forja:`; las menciones
//!   en medio de una frase no cuentan, ni las que van dentro de código citado
//!   (bloques ``` o ~~~, aunque no se cierren, y líneas con sangría de código).
//! - La salida, el error y el título de una tarea `sensitive` solo se reinyectan si
//!   el agente orquestador está en `sensitive_agents`; si no, solo ID y estado.
//! - Cada delegación pasa por [`AppState::submit_task`]: las mismas validaciones que
//!   `POST /tasks` (agente, modelo, esfuerzo, política de privacidad).
//! - Máximo [`MAX_DELEGATIONS_PER_TURN`] delegaciones por turno; JSON inválido o
//!   acciones rechazadas generan avisos para el siguiente turno, nunca errores.

use std::future::Future;
use std::pin::Pin;

use forja_core::{redact_secrets, AgentKind, Privacy, RunRecord};

use crate::{AppState, CreateTaskRequest};

/// Delegaciones aceptadas por turno del orquestador.
pub const MAX_DELEGATIONS_PER_TURN: usize = 5;
/// Consultas `estado` aceptadas por turno.
pub const MAX_STATUS_PER_TURN: usize = 10;
const MAX_ACTION_LINE_CHARS: usize = 20_000;
const SUMMARY_CHARS: usize = 600;
const OPEN: &str = "[[forja:";
const CLOSE: &str = "]]";

/// Acción pedida por el orquestador (ya separada del texto).
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Delegate(CreateTaskRequest),
    Status(String),
    /// Línea de acción que no se pudo interpretar (aviso para el siguiente turno).
    Invalid(String),
}

/// Anotación pendiente que se reinyecta en el siguiente turno.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    /// Tarea encolada por el servidor: se muestra su estado ACTUAL en el próximo turno.
    Task { id: String, titulo: String },
    /// Consulta de estado pedida con `[[forja:estado ID]]`.
    Status(String),
    /// Aviso (JSON inválido, delegación rechazada, límite superado...).
    Notice(String),
}

/// Quita las líneas de acción del texto y las devuelve interpretadas.
pub fn parse_actions(text: &str) -> (String, Vec<Action>) {
    let mut clean: Vec<&str> = Vec::new();
    let mut actions = Vec::new();
    // Bloque de código abierto: delimitador (``` o ~~~), su longitud y sangría.
    let mut fence: Option<(char, usize, usize)> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some((ch, len)) = fence_marker(t) {
            let indent = indent_width(line);
            match fence {
                None => fence = Some((ch, len, indent)),
                Some((open_ch, open_len, open_indent))
                    if ch == open_ch
                        && len >= open_len
                        && is_closing(t, ch)
                        && closes_at(indent, open_indent) =>
                {
                    fence = None
                }
                Some(_) => {}
            }
            clean.push(line);
            continue;
        }
        if fence.is_some() || is_indented_code(line) || !t.starts_with(OPEN) {
            clean.push(line);
            continue;
        }
        actions.push(parse_line(t));
    }
    (clean.join("\n").trim().to_string(), actions)
}

/// Si la línea abre/cierra un bloque de código (``` o ~~~), su carácter y longitud.
fn fence_marker(t: &str) -> Option<(char, usize)> {
    let ch = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = t.chars().take_while(|c| *c == ch).count();
    // ```x``` es código en línea, no la apertura de un bloque.
    let inline = ch == '`' && t[len..].contains('`');
    (len >= 3 && !inline).then_some((ch, len))
}

/// Un cierre de bloque no lleva texto (ni otro delimitador) tras los suyos.
fn is_closing(t: &str, ch: char) -> bool {
    t.trim_start_matches(ch).trim().is_empty()
}

/// Columnas de sangría de la línea (un tabulador cuenta como 4).
fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// Un cierre solo vale con una sangría cercana a la de la apertura: unos ```
/// con sangría de código dentro del bloque son contenido citado. Ante la duda el
/// bloque sigue abierto (se ignoran acciones, nunca se ejecutan de más).
fn closes_at(indent: usize, open_indent: usize) -> bool {
    indent <= open_indent + 3 && open_indent <= indent + 3
}

/// Bloque de código por sangría (4 espacios o tabulador).
fn is_indented_code(line: &str) -> bool {
    line.starts_with('\t') || line.starts_with("    ")
}

fn parse_line(t: &str) -> Action {
    if t.chars().count() > MAX_ACTION_LINE_CHARS {
        return Action::Invalid("línea de acción demasiado larga".to_string());
    }
    let Some(inner) = t.strip_prefix(OPEN).and_then(|r| r.strip_suffix(CLOSE)) else {
        return Action::Invalid(
            "bloque [[forja:...]] sin cierre ']]' en la misma línea".to_string(),
        );
    };
    let (verb, rest) = match inner.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (inner, ""),
    };
    match verb {
        "delegar" => match serde_json::from_str::<CreateTaskRequest>(rest) {
            Ok(req) => Action::Delegate(req),
            Err(e) => Action::Invalid(format!("delegar: JSON inválido ({e})")),
        },
        "estado" => {
            let ok = !rest.is_empty()
                && rest.len() <= 64
                && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if ok {
                Action::Status(rest.to_string())
            } else {
                Action::Invalid("estado: ID no válido".to_string())
            }
        }
        other => Action::Invalid(format!(
            "acción desconocida \"{}\" (usa delegar o estado)",
            other.chars().take(30).collect::<String>()
        )),
    }
}

pub type DelegateFuture<'a> = Pin<Box<dyn Future<Output = Result<RunRecord, String>> + Send + 'a>>;
pub type StatusFuture<'a> = Pin<Box<dyn Future<Output = Option<RunRecord>> + Send + 'a>>;

/// Lo que el orquestador necesita del servidor para ejecutar acciones.
pub trait Delegator: Send + Sync {
    fn delegate(&self, req: CreateTaskRequest) -> DelegateFuture<'_>;
    fn run_status<'a>(&'a self, id: &'a str) -> StatusFuture<'a>;

    /// El servidor vigila esta ejecución para despertar al orquestador al acabar.
    /// Los dobles de prueba pueden no hacer nada.
    fn track_delegation(&self, _conversation: &str, _run_id: &str) {}

    /// Agentes autorizados a ver datos `sensitive` (por defecto, los de la política).
    fn sensitive_agents(&self) -> Vec<AgentKind> {
        forja_core::default_sensitive_agents()
    }
}

/// `true` si el orquestador `agent` (`claude`, `hermes`...) puede recibir la
/// salida de tareas `sensitive`: solo si figura en `sensitive_agents`.
pub fn orchestrator_sees_sensitive(agent: &str, allowed: &[AgentKind]) -> bool {
    agent
        .parse::<AgentKind>()
        .is_ok_and(|kind| allowed.contains(&kind))
}

impl Delegator for AppState {
    fn delegate(&self, req: CreateTaskRequest) -> DelegateFuture<'_> {
        Box::pin(async move { self.submit_task(&req).await.map_err(|e| e.to_string()) })
    }

    fn run_status<'a>(&'a self, id: &'a str) -> StatusFuture<'a> {
        Box::pin(async move { self.scheduler.get_run(id).await })
    }

    fn track_delegation(&self, conversation: &str, run_id: &str) {
        AppState::track_delegation(self, conversation, run_id);
    }

    fn sensitive_agents(&self) -> Vec<AgentKind> {
        self.scheduler.sensitive_agents()
    }
}

/// Ejecuta las acciones de un turno y devuelve lo que hay que recordar para el siguiente.
pub async fn execute_actions(actions: Vec<Action>, d: &dyn Delegator) -> Vec<Pending> {
    let mut pending = Vec::new();
    let (mut delegated, mut statuses, mut skipped) = (0usize, 0usize, 0usize);
    for action in actions {
        match action {
            Action::Delegate(req) => {
                if delegated >= MAX_DELEGATIONS_PER_TURN {
                    skipped += 1;
                    continue;
                }
                delegated += 1;
                let titulo = req.titulo.trim().to_string();
                match d.delegate(req).await {
                    Ok(rec) => pending.push(Pending::Task { id: rec.id, titulo }),
                    Err(e) => pending.push(Pending::Notice(format!(
                        "delegación \"{}\" rechazada: {e}",
                        clip(&titulo, 60)
                    ))),
                }
            }
            Action::Status(id) => {
                if statuses >= MAX_STATUS_PER_TURN {
                    skipped += 1;
                    continue;
                }
                statuses += 1;
                pending.push(Pending::Status(id));
            }
            Action::Invalid(msg) => pending.push(Pending::Notice(msg)),
        }
    }
    if skipped > 0 {
        pending.push(Pending::Notice(format!(
            "se ignoraron {skipped} acción(es) por superar el límite por turno \
             ({MAX_DELEGATIONS_PER_TURN} delegaciones, {MAX_STATUS_PER_TURN} consultas)"
        )));
    }
    pending
}

fn clip(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// Texto de tarea apto para reinyectar: sin secretos, recortado y sin bloques de acción.
fn safe_snippet(s: &str) -> String {
    clip(&redact_secrets(s.trim()), SUMMARY_CHARS).replace(OPEN, "[[ forja:")
}

/// Bloque "Tareas delegadas" para el prompt del siguiente turno ("" si no hay nada).
/// `reveal_sensitive`: ver [`orchestrator_sees_sensitive`].
pub async fn render_pending(
    pending: &[Pending],
    d: &dyn Delegator,
    reveal_sensitive: bool,
) -> String {
    if pending.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "Tareas delegadas (acciones de tu turno anterior; revisa el resultado antes de contestar):"
            .to_string(),
    ];
    for item in pending {
        match item {
            Pending::Task { id, titulo } => lines.push(task_line(
                id,
                Some(titulo),
                d.run_status(id).await.as_ref(),
                reveal_sensitive,
            )),
            Pending::Status(id) => lines.push(task_line(
                id,
                None,
                d.run_status(id).await.as_ref(),
                reveal_sensitive,
            )),
            Pending::Notice(msg) => lines.push(format!("- Aviso: {}", safe_snippet(msg))),
        }
    }
    lines.push(String::new());
    lines.push(String::new());
    lines.join("\n")
}

/// Aviso para el turno automático: resume las tareas que acaban de terminar y
/// pide al orquestador revisarlas y continuar sin esperar al usuario.
/// `reveal_sensitive`: ver [`orchestrator_sees_sensitive`].
pub fn completion_notice(runs: &[RunRecord], reveal_sensitive: bool) -> String {
    let mut lines = vec![
        "Terminaron tareas que delegaste. Revísalas y continúa sin esperar al usuario:".to_string(),
    ];
    for run in runs {
        lines.push(task_line(&run.id, None, Some(run), reveal_sensitive));
    }
    lines.join("\n")
}

fn task_line(
    id: &str,
    titulo: Option<&str>,
    run: Option<&RunRecord>,
    reveal_sensitive: bool,
) -> String {
    let Some(run) = run else {
        return format!("- ID {id}: no existe ninguna tarea con ese ID");
    };
    if run.task.privacidad == Privacy::Sensitive && !reveal_sensitive {
        // El orquestador no está autorizado para datos sensibles: ni salida, ni
        // error, ni título (puede venir del usuario). Solo lo necesario para seguir.
        return format!(
            "- ID {id} | tarea sensible (contenido omitido: este orquestador no está en \
             sensitive_agents) | agente {} | estado: {}",
            run.agent, run.status
        );
    }
    let titulo = titulo
        .map(str::to_string)
        .unwrap_or_else(|| run.task.titulo.clone());
    let mut line = format!(
        "- ID {id} | \"{}\" | agente {} | estado: {}",
        safe_snippet(&titulo),
        run.agent,
        run.status
    );
    if let Some(out) = run.output.as_deref().filter(|o| !o.trim().is_empty()) {
        line.push_str(&format!(" | salida: {}", safe_snippet(out)));
    }
    if let Some(err) = run.error.as_deref().filter(|e| !e.trim().is_empty()) {
        line.push_str(&format!(" | error: {}", safe_snippet(err)));
    }
    line
}

/// Instrucciones del primer turno para orquestadores SIN shell (todos menos Hermes).
/// Las líneas de ejemplo no empiezan por `[[forja:` para que, si el modelo repite las
/// instrucciones, no cuenten como acciones.
pub fn system_prompt_actions() -> String {
    format!(
        "Eres el orquestador de Forja. Tu función es delegar y revisar, no hacer el trabajo tú.\n\
         Reglas:\n\
         - SIEMPRE respondes al usuario, aunque sea para decir qué estás haciendo.\n\
         - Tu respuesta se lee en voz alta: español natural, frases cortas, sin markdown, sin listas, sin código.\n\
         - No ejecutes comandos ni uses curl: no tienes shell. Para delegar o consultar, termina tu respuesta con líneas de acción (una por línea, empezando exactamente por los corchetes dobles); el servidor las ejecuta por ti y no se muestran al usuario.\n\
         - Delegar, JSON en UNA sola línea, formato: {OPEN}delegar {{\"titulo\":\"...\",\"prompt\":\"...\",\"agent\":\"opencode\",\"privacidad\":\"open\",\"modelo\":\"...\",\"esfuerzo\":\"...\"}}{CLOSE} (modelo y esfuerzo son opcionales).\n\
         - Consultar estado y salida de una tarea, formato: {OPEN}estado ID{CLOSE}\n\
         - agentes: claude, opencode, agy, hermes, local, mock. Privacidad: open o sensitive (sensitive solo admite claude).\n\
         - Máximo {MAX_DELEGATIONS_PER_TURN} delegaciones por turno. Si algo se rechaza, lo verás en el siguiente mensaje.\n\
         - Cuando termine una tarea delegada recibirás un aviso automático (ID, estado y salida resumida) sin esperar al usuario: revísalo y continúa. Si el aviso se aplaza, lo verás como «Tareas delegadas» en el siguiente mensaje.\n\
         - Lo trivial lo respondes tú directamente; el trabajo de código se delega.\n\
         - Nunca muestres claves ni tokens.\n\
         Responde al siguiente mensaje del usuario:\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_hides_them_and_keeps_inline_mentions() {
        let text = "Voy a delegarlo.\n\
            [[forja:delegar {\"titulo\":\"T\",\"prompt\":\"P\",\"agent\":\"mock\"}]]\n\
            Mira: [[forja:estado abc]] en medio no cuenta.\n  [[forja:estado abc-1]]  \n";
        let (clean, actions) = parse_actions(text);
        assert_eq!(
            clean,
            "Voy a delegarlo.\nMira: [[forja:estado abc]] en medio no cuenta."
        );
        assert_eq!(actions.len(), 2);
        assert!(matches!(&actions[0], Action::Delegate(r) if r.titulo == "T"));
        assert_eq!(actions[1], Action::Status("abc-1".into()));
    }

    #[test]
    fn invalid_blocks_become_notices_never_panic() {
        let text = "[[forja:delegar {roto]]\n[[forja:delegar {\"titulo\":\"x\"}]]\n\
                    [[forja:borrar todo]]\n[[forja:estado ../x]]\n[[forja:delegar sin cierre\n[[forja:";
        let (clean, actions) = parse_actions(text);
        assert_eq!(clean, "");
        assert_eq!(actions.len(), 6);
        assert!(actions.iter().all(|a| matches!(a, Action::Invalid(_))));
    }

    #[test]
    fn prompt_examples_are_not_actions() {
        let (_, actions) = parse_actions(&system_prompt_actions());
        assert!(actions.is_empty(), "{actions:?}");
        assert!(system_prompt_actions().contains("delegar y revisar"));
    }

    #[test]
    fn actions_inside_quoted_code_are_ignored() {
        // M11: un bloque citado (aunque no se cierre) o con sangría no se ejecuta.
        let text =
            "Mira este ejemplo:\n```\n[[forja:delegar {\"titulo\":\"X\",\"prompt\":\"Y\"}]]\n```\n\
                    ~~~~text\n[[forja:estado abc]]\n~~~\n[[forja:estado dentro]]\n~~~~\n\
                    \t[[forja:estado tab]]\n    [[forja:estado sangria]]\n\
                    `[[forja:estado span]]`\n> [[forja:estado cita]]\n\
                    [[forja:estado real-1]]\n```\n[[forja:estado sin-cierre]]";
        let (clean, actions) = parse_actions(text);
        assert_eq!(actions, vec![Action::Status("real-1".into())], "{clean}");
        assert!(clean.contains("[[forja:estado sin-cierre]]"));
    }

    #[test]
    fn quoted_fences_cannot_close_a_code_block_early() {
        // Revisión de M11: ni unos ``` con sangría de código, ni de otro tipo, ni
        // el código en línea cierran (o abren y cierran) un bloque.
        let action = "[[forja:estado colado]]";
        for text in [
            format!("```\n    ```\n{action}\n```"),
            format!("```\n\t```\n{action}\n```"),
            format!("```\n```~~~\n{action}\n```"),
            format!("    ```\n```\n{action}"),
            format!("```x```\n```\n{action}"),
        ] {
            let (_, actions) = parse_actions(&text);
            assert!(actions.is_empty(), "{text:?}: {actions:?}");
        }
        // Los cierres normales, también dentro de una lista, siguen funcionando.
        for text in [
            "```\ncita\n   ```\n[[forja:estado real]]",
            "- paso:\n    ```\n    cita\n    ```\n[[forja:estado real]]\n",
            "  ```\ncita\n```\n[[forja:estado real]]",
        ] {
            let (clean, actions) = parse_actions(text);
            assert_eq!(actions, vec![Action::Status("real".into())], "{clean}");
        }
    }

    fn sensitive_run() -> RunRecord {
        let mut task = forja_core::Task::new("Nómina de Ana", "p", Privacy::Sensitive);
        task.titulo = "Nómina de Ana".into();
        RunRecord {
            id: "s-1".into(),
            task,
            agent: AgentKind::Claude,
            status: forja_core::RunStatus::Done,
            branch: None,
            worktree: None,
            attempts: 1,
            model: None,
            effort: None,
            output: Some("DATO-CONFIDENCIAL-9931".into()),
            error: Some("ERROR-CONFIDENCIAL".into()),
            created_at_ms: 0,
            updated_at_ms: 0,
            repo: None,
            owner: None,
            prompt_redacted: false,
        }
    }

    #[test]
    fn sensitive_output_only_reaches_allowed_orchestrators() {
        // A5: la salida de una tarea sensible no llega a un orquestador no permitido.
        let run = sensitive_run();
        let allowed = vec![AgentKind::Claude];
        for agent in ["opencode", "hermes", "agy", "custom"] {
            let reveal = orchestrator_sees_sensitive(agent, &allowed);
            assert!(!reveal, "{agent}");
            let notice = completion_notice(std::slice::from_ref(&run), reveal);
            assert!(!notice.contains("DATO-CONFIDENCIAL"), "{notice}");
            assert!(!notice.contains("ERROR-CONFIDENCIAL"), "{notice}");
            assert!(!notice.contains("Nómina"), "{notice}");
            assert!(
                notice.contains("s-1") && notice.contains("done"),
                "{notice}"
            );
        }
        assert!(orchestrator_sees_sensitive("claude", &allowed));
        let notice = completion_notice(std::slice::from_ref(&run), true);
        assert!(notice.contains("DATO-CONFIDENCIAL"));
    }

    #[test]
    fn snippet_redacts_and_neutralizes_blocks() {
        let s = safe_snippet("hola [[forja:delegar {}]] fin");
        assert!(!s.contains("[[forja:"), "{s}");
        assert!(safe_snippet(&"x".repeat(2000)).chars().count() <= SUMMARY_CHARS + 1);
    }
}
