use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use forja_agent::{Agent, AgyAgent, MockAgent};
use forja_core::{AgentKind, ForjaError, Privacy, RunStatus, Task};
use forja_store::JsonlStore;
use tempfile::TempDir;

use super::{init_temp_repo, FlakyAgent};
use crate::{Scheduler, SchedulerConfig};

#[tokio::test]
async fn scheduler_creates_git_worktree_and_commits_changes() {
    let repo_dir = init_temp_repo();
    let store_dir = TempDir::new().expect("crear store tempdir");
    let wt_dir = TempDir::new().expect("crear worktrees tempdir");
    let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

    let mut config = SchedulerConfig::new(wt_dir.path());
    config.repo_path = Some(repo_dir.path().to_path_buf());
    config.base_branch = "main".to_string();

    let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");

    let agent: Arc<dyn Agent> = Arc::new(
        MockAgent::success("archivo creado").with_file("salida.txt", "hola desde forja\n"),
    );

    let task = Task::new("Crear archivo", "Escribe salida.txt", Privacy::Open);
    let submitted = scheduler.submit(task, agent).await.expect("submit");
    let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");

    assert_eq!(finished.status, RunStatus::Done);
    assert_eq!(finished.output.as_deref(), Some("archivo creado"));
    let wt_path = PathBuf::from(finished.worktree.as_ref().expect("worktree registrado"));
    assert!(wt_path.join("salida.txt").is_file());
    assert!(
        forja_git::status_clean(&wt_path).expect("status_clean en worktree"),
        "el worktree debe quedar limpio tras el commit automático"
    );
}

#[tokio::test]
async fn scheduler_persists_failed_run_error() {
    let store_dir = TempDir::new().expect("crear store tempdir");
    let wt_dir = TempDir::new().expect("crear worktrees tempdir");
    let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

    let config = SchedulerConfig::new(wt_dir.path());
    let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");

    let failing_agent: Arc<dyn Agent> = Arc::new(MockAgent::fail("fallo simulado"));
    let task = Task::new("Tarea con fallo", "Falla", Privacy::Open);
    let submitted = scheduler.submit(task, failing_agent).await.expect("submit");
    let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");

    assert_eq!(finished.status, RunStatus::Failed);
    assert!(finished
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("fallo simulado"));

    let stored = store
        .get_run(&submitted.id)
        .expect("get_run")
        .expect("debe existir en store");
    assert_eq!(stored.status, RunStatus::Failed);
    assert_eq!(stored.error, finished.error);
}

#[tokio::test]
async fn policy_denies_sensitive_with_agy_without_creating_worktree() {
    let repo_dir = init_temp_repo();
    let store_dir = TempDir::new().expect("crear store tempdir");
    let wt_dir = TempDir::new().expect("crear worktrees tempdir");
    let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

    let mut config = SchedulerConfig::new(wt_dir.path());
    config.repo_path = Some(repo_dir.path().to_path_buf());
    config.base_branch = "main".to_string();

    let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");
    let agent: Arc<dyn Agent> = Arc::new(AgyAgent::default());
    let task = Task::new("Dato confidencial", "Procesar secreto", Privacy::Sensitive);

    let err = scheduler
        .submit(task, agent)
        .await
        .expect_err("submit debe rechazar Sensitive + Agy");

    assert!(
        matches!(
            err,
            ForjaError::PolicyDenied {
                privacy: Privacy::Sensitive,
                agent: AgentKind::Agy
            }
        ),
        "error inesperado: {err:?}"
    );

    assert!(
        fs::read_dir(wt_dir.path())
            .expect("leer worktrees_dir")
            .next()
            .is_none(),
        "worktrees_dir debe permanecer vacío tras rechazo de política"
    );

    let branches_output = Command::new("git")
        .arg("-C")
        .arg(repo_dir.path())
        .args(["branch", "--list", "forja/*"])
        .output()
        .expect("listar ramas git");
    assert!(branches_output.status.success());
    assert!(
        String::from_utf8_lossy(&branches_output.stdout)
            .trim()
            .is_empty(),
        "no debe haberse creado ninguna rama forja/*"
    );

    assert!(scheduler.list_runs().await.is_empty());
    assert!(store.list_runs().expect("listar store").is_empty());
}

#[tokio::test]
async fn retry_succeeds_on_third_attempt() {
    let store_dir = TempDir::new().expect("crear store tempdir");
    let wt_dir = TempDir::new().expect("crear worktrees tempdir");
    let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

    let mut config = SchedulerConfig::new(wt_dir.path());
    config.max_retries = 2;
    config.retry_backoff = Duration::from_millis(10);

    let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");
    let agent: Arc<dyn Agent> = Arc::new(FlakyAgent::new(2));
    let task = Task::new("Tarea inestable", "Reintentar hasta éxito", Privacy::Open);

    let submitted = scheduler.submit(task, agent).await.expect("submit");
    let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");

    assert_eq!(finished.status, RunStatus::Done);
    assert_eq!(finished.attempts, 3);
    assert_eq!(finished.output.as_deref(), Some("exito en intento 3"));
    assert!(finished.error.is_none());

    let stored = store
        .get_run(&submitted.id)
        .expect("get_run")
        .expect("run en store");
    assert_eq!(stored.status, RunStatus::Done);
    assert_eq!(stored.attempts, 3);
}

#[tokio::test]
async fn opencode_variant_fallback_is_not_a_failed_attempt_and_clears_effort() {
    let store_dir = TempDir::new().expect("store");
    let wt_dir = TempDir::new().expect("wt");
    let store = JsonlStore::new(store_dir.path()).expect("JsonlStore");
    let scheduler = Scheduler::new(store, SchedulerConfig::new(wt_dir.path())).expect("Scheduler");
    std::env::set_var("FAKE_OPENCODE_NO_VARIANTS", "1");
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../forja-agent/tests/fake_opencode.sh");
    let agent: Arc<dyn Agent> = Arc::new(
        forja_agent::OpencodeAgent::default()
            .with_program(script.to_string_lossy())
            .with_effort("high"),
    );
    let task = Task::new("t", "p", Privacy::Open);
    let submitted = scheduler.submit(task, agent).await.expect("submit");
    assert_eq!(submitted.effort.as_deref(), Some("high"));
    let finished = scheduler.wait_run(&submitted.id).await.expect("wait");
    std::env::remove_var("FAKE_OPENCODE_NO_VARIANTS");
    assert_eq!(finished.status, RunStatus::Done, "{:?}", finished.error);
    assert_eq!(finished.attempts, 1);
    assert_eq!(finished.effort, None);
    let out = finished.output.unwrap();
    assert!(out.contains("se ejecutó sin variante"), "{out}");
}

#[tokio::test]
async fn retry_exhausted_marks_failed() {
    let store_dir = TempDir::new().expect("crear store tempdir");
    let wt_dir = TempDir::new().expect("crear worktrees tempdir");
    let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

    let mut config = SchedulerConfig::new(wt_dir.path());
    config.max_retries = 2;
    config.retry_backoff = Duration::from_millis(10);

    let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");
    let agent: Arc<dyn Agent> = Arc::new(FlakyAgent::new(u32::MAX));
    let task = Task::new("Tarea siempre fallida", "Falla siempre", Privacy::Open);

    let submitted = scheduler.submit(task, agent).await.expect("submit");
    let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");

    assert_eq!(finished.status, RunStatus::Failed);
    assert_eq!(finished.attempts, 3);
    assert!(finished
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("fallo en intento 3"));

    let stored = store
        .get_run(&submitted.id)
        .expect("get_run")
        .expect("run en store");
    assert_eq!(stored.status, RunStatus::Failed);
    assert_eq!(stored.attempts, 3);
    assert_eq!(stored.error, finished.error);
}

#[tokio::test]
async fn custom_sensitive_agents_local_only_and_hot_update() {
    let store_dir = TempDir::new().expect("crear store tempdir");
    let wt_dir = TempDir::new().expect("crear worktrees tempdir");
    let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

    let mut config = SchedulerConfig::new(wt_dir.path());
    config.sensitive_agents = vec![AgentKind::Local];
    let scheduler = Scheduler::new(store, config).expect("crear Scheduler");
    assert_eq!(scheduler.sensitive_agents(), vec![AgentKind::Local]);

    // Con ["local"], agy y claude se deniegan para sensitive.
    let agy: Arc<dyn Agent> = Arc::new(AgyAgent::default());
    let err_agy = scheduler
        .submit(Task::new("t1", "p", Privacy::Sensitive), agy.clone())
        .await
        .expect_err("agy debe ser denegado");
    assert!(matches!(
        err_agy,
        ForjaError::PolicyDenied {
            privacy: Privacy::Sensitive,
            agent: AgentKind::Agy
        }
    ));

    let claude: Arc<dyn Agent> = Arc::new(forja_agent::ClaudeAgent::default());
    let err_claude = scheduler
        .submit(Task::new("t2", "p", Privacy::Sensitive), claude)
        .await
        .expect_err("claude debe ser denegado cuando solo local está permitido");
    assert!(matches!(
        err_claude,
        ForjaError::PolicyDenied {
            privacy: Privacy::Sensitive,
            agent: AgentKind::Claude
        }
    ));

    // local sí se acepta con ["local"].
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener efímero");
    let port = listener.local_addr().expect("addr").port();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../forja-agent/tests/fake_local.sh");
    let local: Arc<dyn Agent> = Arc::new(
        forja_agent::LocalAgent::default()
            .with_program(script.to_string_lossy())
            .with_port(port),
    );
    let submitted = scheduler
        .submit(Task::new("t3", "p", Privacy::Sensitive), local)
        .await
        .expect("local debe ser aceptado");
    let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");
    drop(listener);
    assert_eq!(finished.status, RunStatus::Done);

    // Cambio en caliente sin reiniciar: ahora permite mock y rechaza listas inválidas.
    assert!(scheduler.set_sensitive_agents(vec![]).is_err());
    assert!(scheduler
        .set_sensitive_agents(vec![AgentKind::Mock, AgentKind::Mock])
        .is_err());

    let mock: Arc<dyn Agent> = Arc::new(MockAgent::success("ok"));
    assert!(matches!(
        scheduler
            .submit(Task::new("t4", "p", Privacy::Sensitive), mock.clone())
            .await,
        Err(ForjaError::PolicyDenied {
            privacy: Privacy::Sensitive,
            agent: AgentKind::Mock
        })
    ));

    scheduler
        .set_sensitive_agents(vec![AgentKind::Mock])
        .expect("actualizar sensitive_agents en caliente");
    assert_eq!(scheduler.sensitive_agents(), vec![AgentKind::Mock]);

    let submitted = scheduler
        .submit(Task::new("t5", "p", Privacy::Sensitive), mock)
        .await
        .expect("mock debe aceptarse tras cambio en caliente");
    let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");
    assert_eq!(finished.status, RunStatus::Done);
}
