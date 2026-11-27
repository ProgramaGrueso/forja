//! Regresiones del test adversarial (docs/adversarial/ENTREGA.md): A4, B4, M2,
//! M4, M5 y M10 en el planificador.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forja_agent::{Agent, AgentFuture, AgentOutput, MockAgent, RunContext};
use forja_core::limits::{MAX_PROMPT_BYTES, MAX_STORED_OUTPUT_BYTES};
use forja_core::{AgentKind, ForjaError, Privacy, RunRecord, RunStatus, Task};
use forja_store::JsonlStore;
use tempfile::TempDir;

use super::{init_temp_repo, new_scheduler, open_task};
use crate::{Scheduler, SchedulerConfig, INTERRUPTED_ERROR};

const FAKE_KEY: &str = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd";

/// Agente que guarda el prompt recibido y responde con un texto fijo.
struct CaptureAgent {
    seen: Arc<Mutex<Option<String>>>,
    reply: String,
}

impl Agent for CaptureAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Mock
    }

    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
        *self.seen.lock().unwrap() = Some(ctx.task.prompt.clone());
        let reply = self.reply.clone();
        Box::pin(async move {
            Ok(AgentOutput {
                text: reply.clone(),
                raw_stdout: reply,
                raw_stderr: String::new(),
                exit_code: Some(0),
                warnings: Vec::new(),
            })
        })
    }
}

/// Agente que no termina nunca por sí solo.
struct HangingAgent;

impl Agent for HangingAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Mock
    }

    fn run<'a>(&'a self, _ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Err(ForjaError::Agent("inalcanzable".to_string()))
        })
    }
}

fn stored_record(id: &str, status: RunStatus, owner: Option<String>) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        task: Task::new("vieja", "prompt", Privacy::Open),
        agent: AgentKind::Mock,
        status,
        branch: None,
        worktree: None,
        attempts: 1,
        model: None,
        effort: None,
        output: None,
        error: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        repo: None,
        owner,
        prompt_redacted: false,
    }
}

#[tokio::test]
async fn sensitive_local_task_with_cloud_model_is_rejected() {
    // A4: con `local` permitido para sensitive, un modelo de nube no pasa.
    let store_dir = TempDir::new().unwrap();
    let wt_dir = TempDir::new().unwrap();
    let mut config = SchedulerConfig::new(wt_dir.path());
    config.sensitive_agents = vec![AgentKind::Local];
    let scheduler = Scheduler::new(JsonlStore::new(store_dir.path()).unwrap(), config).unwrap();

    let cloud: Arc<dyn Agent> = Arc::new(forja_agent::LocalAgent::new(
        "openrouter/vendor/modelo-nube",
    ));
    let err = scheduler
        .submit(Task::new("t", "DATO", Privacy::Sensitive), cloud)
        .await
        .expect_err("modelo de nube en sensitive debe rechazarse");
    assert!(matches!(err, ForjaError::InvalidInput(_)), "{err:?}");
    assert!(
        scheduler.list_runs().await.is_empty(),
        "no debe crearse run"
    );
}

#[tokio::test]
async fn oversized_prompt_is_rejected_with_clear_error() {
    // B4: prompt mayor que el tope => error antes de lanzar nada.
    let (scheduler, _s, _w) = new_scheduler(1);
    let prompt = "x".repeat(MAX_PROMPT_BYTES + 1);
    let agent: Arc<dyn Agent> = Arc::new(MockAgent::success("ok"));
    let err = scheduler
        .submit(Task::new("grande", prompt, Privacy::Open), agent)
        .await
        .expect_err("prompt demasiado grande");
    assert!(err.to_string().contains("KiB"), "{err}");
    assert!(scheduler.list_runs().await.is_empty());
}

#[tokio::test]
async fn huge_output_is_stored_truncated_with_mark() {
    // B4: la salida guardada tiene tope y marca de truncado.
    let (scheduler, store_dir, _w) = new_scheduler(1);
    let reply = "a".repeat(MAX_STORED_OUTPUT_BYTES * 4);
    let agent: Arc<dyn Agent> = Arc::new(CaptureAgent {
        seen: Arc::default(),
        reply,
    });
    let run = scheduler
        .submit(Task::new("t", "p", Privacy::Open), agent)
        .await
        .unwrap();
    let done = scheduler.wait_run(&run.id).await.unwrap();
    let out = done.output.unwrap();
    assert!(out.len() < MAX_STORED_OUTPUT_BYTES + 200, "{}", out.len());
    assert!(out.contains("salida truncada"));
    let jsonl = std::fs::metadata(store_dir.path().join("runs.jsonl")).unwrap();
    assert!(jsonl.len() < (MAX_STORED_OUTPUT_BYTES as u64) * 2);
}

#[tokio::test]
async fn secrets_are_redacted_before_persisting_but_agent_gets_original() {
    // M10: ni la memoria, ni el store, ni la salida guardan la clave.
    let (scheduler, store_dir, _w) = new_scheduler(1);
    let seen = Arc::new(Mutex::new(None));
    let agent: Arc<dyn Agent> = Arc::new(CaptureAgent {
        seen: Arc::clone(&seen),
        reply: format!("la clave es {FAKE_KEY}"),
    });
    let prompt = format!("usa {FAKE_KEY} para la API");
    let run = scheduler
        .submit(Task::new("t", prompt.clone(), Privacy::Open), agent)
        .await
        .unwrap();
    assert!(run.prompt_redacted);
    assert!(!run.task.prompt.contains(FAKE_KEY));
    let done = scheduler.wait_run(&run.id).await.unwrap();
    assert_eq!(seen.lock().unwrap().as_deref(), Some(prompt.as_str()));
    assert!(!done.output.unwrap().contains(FAKE_KEY));
    let jsonl = std::fs::read_to_string(store_dir.path().join("runs.jsonl")).unwrap();
    assert!(!jsonl.contains(FAKE_KEY), "runs.jsonl conserva la clave");
}

#[tokio::test]
async fn old_records_with_secrets_are_redacted_on_load() {
    let store_dir = TempDir::new().unwrap();
    let wt_dir = TempDir::new().unwrap();
    let store = JsonlStore::new(store_dir.path()).unwrap();
    let mut old = stored_record("vieja-1", RunStatus::Done, None);
    old.output = Some(format!("token {FAKE_KEY}"));
    store.append_run(&old).unwrap();
    let scheduler = Scheduler::new(store, SchedulerConfig::new(wt_dir.path())).unwrap();
    let rec = scheduler.get_run("vieja-1").await.unwrap();
    assert!(!rec.output.unwrap().contains(FAKE_KEY));
}

#[tokio::test]
async fn startup_marks_orphan_active_runs_failed_but_keeps_live_foreign_ones() {
    // M2: runs `running` de un proceso muerto no bloquean para siempre; los de
    // otro proceso vivo (p. ej. un `forja run`) no se tocan.
    let store_dir = TempDir::new().unwrap();
    let wt_dir = TempDir::new().unwrap();
    let store = JsonlStore::new(store_dir.path()).unwrap();

    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let child_identity = forja_agent::process::process_identity(child.id());

    store
        .append_run(&stored_record("legado", RunStatus::Running, None))
        .unwrap();
    store
        .append_run(&stored_record(
            "muerto",
            RunStatus::Pending,
            Some("999999999:1".to_string()),
        ))
        .unwrap();
    store
        .append_run(&stored_record(
            "ajeno-vivo",
            RunStatus::Running,
            Some(child_identity),
        ))
        .unwrap();

    let scheduler = Scheduler::new(store.clone(), SchedulerConfig::new(wt_dir.path())).unwrap();
    for id in ["legado", "muerto"] {
        let rec = scheduler.get_run(id).await.unwrap();
        assert_eq!(rec.status, RunStatus::Failed, "{id}");
        assert_eq!(rec.error.as_deref(), Some(INTERRUPTED_ERROR));
        assert_eq!(
            store.get_run(id).unwrap().unwrap().status,
            RunStatus::Failed
        );
    }
    assert_eq!(
        scheduler.get_run("ajeno-vivo").await.unwrap().status,
        RunStatus::Running
    );
    assert!(scheduler.has_active_runs().await);

    // El otro proceso muere sin cerrar el run: deja de bloquear.
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!scheduler.has_active_runs().await);
    assert_eq!(
        scheduler.get_run("ajeno-vivo").await.unwrap().status,
        RunStatus::Failed
    );
}

#[tokio::test]
async fn shutdown_cancels_own_active_runs_and_persists_it() {
    // M2: apagado ordenado.
    let (scheduler, _s, _w) = new_scheduler(2);
    let agent: Arc<dyn Agent> = Arc::new(HangingAgent);
    let run = scheduler
        .submit(Task::new("larga", "p", Privacy::Open), agent)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    scheduler.shutdown("servidor detenido").await;
    let rec = scheduler.get_run(&run.id).await.unwrap();
    assert_eq!(rec.status, RunStatus::Cancelled);
    assert_eq!(rec.error.as_deref(), Some("servidor detenido"));
    assert!(!scheduler.has_active_runs().await);
}

#[tokio::test]
async fn repo_without_gitignore_stays_clean_with_internal_dirs_inside() {
    // M4: `.forja/` dentro del repo y sin .gitignore no ensucia la base.
    let repo = init_temp_repo();
    let store_path = repo.path().join(".forja/store");
    let store = JsonlStore::new(&store_path).unwrap();
    let mut config = SchedulerConfig::new(repo.path().join(".forja/worktrees"));
    config.repo_path = Some(repo.path().to_path_buf());
    let scheduler = Scheduler::new(store, config).unwrap();
    let agent: Arc<dyn Agent> = Arc::new(MockAgent::success("ok").with_file("a.txt", "a\n"));
    let run = scheduler
        .submit(Task::new("t", "p", Privacy::Open), agent)
        .await
        .unwrap();
    let done = scheduler.wait_run(&run.id).await.unwrap();
    assert_eq!(done.status, RunStatus::Done, "{:?}", done.error);
    assert!(forja_git::status_clean(repo.path()).unwrap());
    assert!(!repo.path().join(".gitignore").exists());
    // El registro sabe a qué repo pertenece (B1).
    let expected = repo.path().canonicalize().unwrap();
    assert_eq!(done.repo.map(PathBuf::from), Some(expected));
}

#[test]
fn relative_worktrees_dir_is_made_absolute() {
    // M5: el cwd posterior no cambia dónde viven los worktrees.
    let store_dir = TempDir::new().unwrap();
    let scheduler = Scheduler::new(
        JsonlStore::new(store_dir.path()).unwrap(),
        SchedulerConfig::new(".forja/worktrees"),
    )
    .unwrap();
    assert!(scheduler.worktrees_dir().is_absolute());
}

#[tokio::test]
async fn idle_guard_refuses_with_active_runs_and_blocks_new_ones() {
    // M1: sin ejecuciones activas hay guard y, mientras viva, no entran nuevas.
    let (scheduler, _s, _w) = new_scheduler(1);
    let running = scheduler
        .submit(open_task("colgada"), Arc::new(HangingAgent))
        .await
        .expect("encolar");
    assert!(scheduler.idle_guard().await.is_none());
    scheduler.cancel_run(&running.id).await.expect("cancelar");

    let guard = scheduler.idle_guard().await.expect("sin activas");
    let other = scheduler.clone();
    let mut pending = tokio::spawn(async move {
        other
            .submit(open_task("espera"), Arc::new(MockAgent::success("ok")))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut pending)
            .await
            .is_err(),
        "no debe darse de alta mientras vive el guard"
    );
    drop(guard);
    pending.await.expect("join").expect("alta tras soltar");
    scheduler.wait_all().await;
}

#[tokio::test]
async fn idle_guard_sees_runs_of_another_live_process() {
    // M1: un `forja run` arrancado después que el servidor también cuenta.
    let (scheduler, store_dir, _w) = new_scheduler(1);
    let mut child = Command::new("sleep").arg("30").spawn().expect("sleep");
    let owner = forja_agent::process::process_identity(child.id());
    JsonlStore::new(store_dir.path())
        .expect("store")
        .append_run(&stored_record("ajena", RunStatus::Running, Some(owner)))
        .expect("append");
    let busy = scheduler.idle_guard().await.is_none();
    let switched = scheduler.set_workspace(None, "main".to_string()).await;
    let active = scheduler.has_active_runs().await;
    child.kill().expect("kill");
    child.wait().expect("wait");
    assert!(busy, "hay un run activo de otro proceso vivo");
    assert!(active);
    assert!(
        switched.is_err(),
        "no se cambia de carpeta con ese run vivo"
    );
    assert!(scheduler.idle_guard().await.is_some());
}
