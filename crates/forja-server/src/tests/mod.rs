mod adversarial;
mod auth_security;
mod files_workspaces;
mod panel_mascot;
mod retry_redact;
mod review;
mod review_safety;
mod stream_output;
mod tasks_runs;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request};
use forja_agent::{Agent, MockAgent};
use forja_core::{Privacy, RunRecord, RunStatus};
use forja_sched::{Scheduler, SchedulerConfig};
use forja_store::JsonlStore;
use tempfile::TempDir;
use tower::ServiceExt;

use crate::AppState;

pub(super) fn test_state() -> (AppState, TempDir, TempDir) {
    let store_dir = TempDir::new().expect("crear store tempdir");
    let wt_dir = TempDir::new().expect("crear worktrees tempdir");
    let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");
    let config = SchedulerConfig::new(wt_dir.path());
    let scheduler = Scheduler::new(store, config).expect("crear Scheduler");
    (AppState::new(scheduler), store_dir, wt_dir)
}

pub(super) async fn read_json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, usize::MAX).await.expect("leer cuerpo HTTP");
    serde_json::from_slice(&bytes).expect("deserializar cuerpo JSON")
}

pub(super) fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("ejecutar git en test");
    assert!(
        output.status.success(),
        "git {:?} falló:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn init_git_repo() -> TempDir {
    let dir = TempDir::new().expect("crear repo tempdir");
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.name", "Forja Test"]);
    run_git(
        dir.path(),
        &["config", "user.email", "forja-test@example.com"],
    );
    fs::write(dir.path().join("README.md"), "# Repo de prueba\n").expect("escribir README");
    run_git(dir.path(), &["add", "README.md"]);
    run_git(dir.path(), &["commit", "-m", "commit inicial"]);
    dir
}

/// Estado con repo git real: las tareas crean worktrees y ramas `forja/<id>`.
pub(super) fn test_state_with_repo() -> (AppState, TempDir, TempDir) {
    let repo = init_git_repo();
    let data = TempDir::new().expect("crear datos tempdir");
    let store = JsonlStore::new(data.path().join("store")).expect("crear JsonlStore");
    let mut config = SchedulerConfig::new(data.path().join("worktrees"));
    config.repo_path = Some(repo.path().to_path_buf());
    config.base_branch = "main".to_string();
    let scheduler = Scheduler::new(store, config).expect("crear Scheduler");
    (AppState::new(scheduler), repo, data)
}

pub(super) async fn get(app: &axum::Router, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("respuesta")
}

pub(super) async fn post(app: &axum::Router, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(Method::POST)
                .uri(uri)
                .body(Body::empty())
                .expect("request POST"),
        )
        .await
        .expect("respuesta POST")
}

/// Crea una tarea con MockAgent que escribe un archivo y espera a que acabe.
pub(super) async fn finished_task(
    state: &AppState,
    titulo: &str,
    archivo: &str,
    contenido: &str,
) -> RunRecord {
    let agent: Arc<dyn Agent> =
        Arc::new(MockAgent::success("listo").with_file(archivo.to_string(), contenido.to_string()));
    let run = state
        .scheduler
        .submit(
            forja_core::Task::new(titulo, "prompt", Privacy::Open),
            agent,
        )
        .await
        .expect("encolar tarea");
    let finished = state
        .scheduler
        .wait_run(&run.id)
        .await
        .expect("esperar tarea");
    assert_eq!(finished.status, RunStatus::Done, "{:?}", finished.error);
    finished
}

pub(super) async fn wait_until_running(state: &AppState, id: &str) -> RunRecord {
    for _ in 0..300 {
        if let Some(record) = state.scheduler.get_run(id).await {
            let worktree_ready = record
                .worktree
                .as_deref()
                .is_some_and(|path| Path::new(path).is_dir());
            if record.status == RunStatus::Running && record.branch.is_some() && worktree_ready {
                return record;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("la ejecución no llegó a estado running con worktree");
}
