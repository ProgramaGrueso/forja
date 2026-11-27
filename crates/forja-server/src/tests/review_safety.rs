//! Regresiones del test adversarial en la revisión: A3 (HEAD suelto / otra
//! rama), orden base-limpia-antes-de-commitear, B1 (otro repo) y M4 (.forja/
//! sin .gitignore).

use std::fs;
use std::path::Path;

use axum::http::StatusCode;
use serde_json::Value;

use super::{finished_task, init_git_repo, post, read_json, run_git, test_state_with_repo};
use crate::build_router;

fn commit_count(repo: &Path, rev: &str) -> usize {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--count", rev])
        .output()
        .expect("git rev-list");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[tokio::test]
async fn merge_refuses_detached_head_and_keeps_worktree_and_branch() {
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Suelta", "base.txt", "base\n").await;
    let wt = Path::new(run.worktree.as_deref().unwrap()).to_path_buf();
    // El agente deja HEAD suelto y commitea ahí su trabajo.
    run_git(&wt, &["checkout", "--detach"]);
    fs::write(wt.join("perdido.txt"), "trabajo\n").unwrap();
    run_git(&wt, &["add", "perdido.txt"]);
    run_git(&wt, &["commit", "-m", "trabajo en HEAD suelto"]);

    let res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    let status = res.status();
    let err: Value = read_json(res.into_body()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    let msg = err["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("HEAD suelto") && msg.contains("checkout -B"),
        "{msg}"
    );
    assert!(wt.join("perdido.txt").is_file(), "el worktree debe seguir");
    assert!(forja_git::branch_exists(repo.path(), &format!("forja/{}", run.id)).unwrap());
    assert!(!repo.path().join("perdido.txt").exists());

    // Siguiendo la instrucción del error, la fusión lleva el trabajo a main.
    run_git(&wt, &["checkout", "-B", &format!("forja/{}", run.id)]);
    let res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(repo.path().join("perdido.txt").is_file());
}

#[tokio::test]
async fn merge_refuses_worktree_switched_to_another_branch() {
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Otra rama", "base.txt", "base\n").await;
    let wt = Path::new(run.worktree.as_deref().unwrap()).to_path_buf();
    run_git(&wt, &["checkout", "-b", "experimento"]);
    fs::write(wt.join("exp.txt"), "x\n").unwrap();

    let res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let err: Value = read_json(res.into_body()).await;
    assert!(err["error"].as_str().unwrap_or("").contains("experimento"));
    assert!(wt.join("exp.txt").is_file());
    assert!(
        !forja_git::status_clean(&wt).unwrap(),
        "no debe commitearse nada si se rechaza"
    );
    assert!(forja_git::status_clean(repo.path()).unwrap());
}

#[tokio::test]
async fn merge_checks_dirty_base_before_committing_worktree() {
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Orden", "base.txt", "base\n").await;
    let wt = Path::new(run.worktree.as_deref().unwrap()).to_path_buf();
    let branch = format!("forja/{}", run.id);
    let before = commit_count(repo.path(), &branch);
    fs::write(wt.join("tarde.txt"), "tarde\n").unwrap();
    fs::write(repo.path().join("pendiente.txt"), "sucio\n").unwrap();

    let res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        commit_count(repo.path(), &branch),
        before,
        "un rechazo por base sucia no debe crear commits en la rama"
    );
    assert!(!forja_git::status_clean(&wt).unwrap());
}

#[tokio::test]
async fn merge_works_in_repo_without_forja_in_gitignore() {
    // M4: carpetas internas dentro del repo y sin .gitignore.
    let repo = init_git_repo();
    let store = forja_store::JsonlStore::new(repo.path().join(".forja/store")).unwrap();
    let mut config = forja_sched::SchedulerConfig::new(repo.path().join(".forja/worktrees"));
    config.repo_path = Some(repo.path().to_path_buf());
    let scheduler = forja_sched::Scheduler::new(store, config).unwrap();
    let state = crate::AppState::new(scheduler);
    let app = build_router(state.clone());
    // Simula un repo existente cuyo exclude aún no tiene la entrada.
    let exclude = repo.path().join(".git/info/exclude");
    let run = finished_task(&state, "M4", "m4.txt", "m4\n").await;
    fs::write(&exclude, "").unwrap();
    assert!(!forja_git::status_clean(repo.path()).unwrap(), "?? .forja/");

    let res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(repo.path().join("m4.txt").is_file());
    assert!(!repo.path().join(".gitignore").exists());
    assert!(fs::read_to_string(&exclude).unwrap().contains("/.forja/"));
}

#[tokio::test]
async fn review_of_run_from_another_repo_is_a_clear_conflict() {
    // B1: tras cambiar de carpeta, ni fusionar ni descartar tocan nada.
    let (state, _repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Repo A", "a.txt", "a\n").await;
    let wt = Path::new(run.worktree.as_deref().unwrap()).to_path_buf();

    let other = init_git_repo();
    state
        .scheduler
        .set_workspace(Some(other.path().to_path_buf()), "main".to_string())
        .await
        .unwrap();

    for action in ["merge", "discard"] {
        let res = post(&app, &format!("/runs/{}/{action}", run.id)).await;
        assert_eq!(res.status(), StatusCode::CONFLICT, "{action}");
        let err: Value = read_json(res.into_body()).await;
        assert!(
            err["error"].as_str().unwrap_or("").contains("pertenece"),
            "{action}: {err}"
        );
    }
    assert!(wt.join("a.txt").is_file(), "el worktree sigue intacto");
}

#[tokio::test]
async fn renamed_repo_can_be_managed_again_after_worktree_repair() {
    // Revisión de B1: un repo renombrado no deja sus ejecuciones bloqueadas.
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Movido", "m.txt", "m\n").await;
    let wt = Path::new(run.worktree.as_deref().unwrap()).to_path_buf();

    let parent = tempfile::tempdir().expect("tmp");
    let moved = parent.path().join("renombrado");
    fs::rename(repo.path(), &moved).expect("rename");
    state
        .scheduler
        .set_workspace(Some(moved.clone()), "main".to_string())
        .await
        .unwrap();

    let res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    let status = res.status();
    let err: Value = read_json(res.into_body()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap_or("")
            .contains("worktree repair"),
        "{err}"
    );

    run_git(&moved, &["worktree", "repair", wt.to_str().unwrap()]);
    let res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    let status = res.status();
    let body: Value = read_json(res.into_body()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(moved.join("m.txt").is_file());
}
