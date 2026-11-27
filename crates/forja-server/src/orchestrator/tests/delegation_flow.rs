use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use forja_core::{AgentKind, Privacy};
use forja_sched::{Scheduler, SchedulerConfig};
use forja_store::JsonlStore;
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::delegation::{Delegator, Pending};
use crate::AppState;

use super::*;

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
            forja_agent::MockAgent::success("ok").with_delay(std::time::Duration::from_millis(120)),
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
        tokio::spawn(async move { o.say_in_with(PRINCIPAL, "hola", 7777, None, None).await });
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
    let line = "EMIT: [[forja:delegar {\"titulo\":\"T\",\"prompt\":\"p\",\"agent\":\"mock\"}]]";
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
        crate::delegation::render_pending(&[Pending::Status(rec.id.clone())], &Fixed(run), true)
            .await;
    let (_, actions) = crate::delegation::parse_actions(&line);
    assert!(actions.is_empty(), "{line}");
}

struct Fixed(forja_core::RunRecord);
impl Delegator for Fixed {
    fn delegate(&self, _r: crate::CreateTaskRequest) -> crate::delegation::DelegateFuture<'_> {
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
    let (st, v) = chat(json!({"message": "hola", "modelo": "haiku", "esfuerzo": "medium"})).await;
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
    let (st, _) = chat(json!({"message": "x", "conversation_id": "nope", "modelo": "a"})).await;
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
