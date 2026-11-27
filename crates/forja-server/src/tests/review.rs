use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use forja_agent::{Agent, MockAgent};
use forja_core::{Privacy, RunStatus};
use serde_json::Value;

use super::{
    finished_task, get, post, read_json, test_state, test_state_with_repo, wait_until_running,
};
use crate::build_router;

#[tokio::test]
async fn review_diff_then_merge_closes_worktree_cycle() {
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Feature", "feature.txt", "hola mundo\n").await;
    let worktree = Path::new(run.worktree.as_deref().expect("worktree registrado"));
    assert!(worktree.is_dir(), "el worktree debe existir");

    let diff_res = get(&app, &format!("/runs/{}/diff", run.id)).await;
    assert_eq!(diff_res.status(), StatusCode::OK);
    let diff: Value = read_json(diff_res.into_body()).await;
    assert_eq!(diff["branch"], format!("forja/{}", run.id));
    assert_eq!(diff["base"], "main");
    assert!(
        diff["diff"].as_str().unwrap_or("").contains("feature.txt"),
        "el diff debe listar el archivo nuevo: {diff}"
    );
    assert!(diff["diff"].as_str().unwrap_or("").contains("+hola mundo"));
    assert!(diff["stat"].as_str().unwrap_or("").contains("feature.txt"));

    let merge_res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(merge_res.status(), StatusCode::OK);
    let merged: Value = read_json(merge_res.into_body()).await;
    assert_eq!(merged["merged"], true);
    assert_eq!(merged["worktree_removed"], true);
    assert_eq!(merged["branch_deleted"], true);
    assert!(merged["commit"].as_str().is_some_and(|c| !c.is_empty()));
    assert_eq!(
        fs::read_to_string(repo.path().join("feature.txt")).expect("archivo fusionado"),
        "hola mundo\n"
    );
    assert!(
        !worktree.exists(),
        "el worktree debe retirarse tras fusionar"
    );
    assert!(
        forja_git::status_clean(repo.path()).expect("status de la base"),
        "la rama base debe quedar limpia"
    );

    let again = get(&app, &format!("/runs/{}/diff", run.id)).await;
    assert_eq!(
        again.status(),
        StatusCode::CONFLICT,
        "un ciclo cerrado no debe seguir mostrando diff"
    );
    let err: Value = read_json(again.into_body()).await;
    assert!(err["error"].as_str().unwrap_or("").contains("worktree"));
}

#[tokio::test]
async fn review_discard_removes_worktree_branch_and_changes() {
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Descartable", "desecho.txt", "no fusionar\n").await;
    let worktree = Path::new(run.worktree.as_deref().expect("worktree registrado"));

    let discard_res = post(&app, &format!("/runs/{}/discard", run.id)).await;
    assert_eq!(discard_res.status(), StatusCode::OK);
    let discarded: Value = read_json(discard_res.into_body()).await;
    assert_eq!(discarded["merged"], false);
    assert_eq!(discarded["worktree_removed"], true);
    assert_eq!(discarded["branch_deleted"], true);
    assert!(!worktree.exists(), "el worktree debe desaparecer");
    assert!(
        !repo.path().join("desecho.txt").exists(),
        "el trabajo descartado no debe llegar a la rama base"
    );
    let branch = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/forja/{}", run.id))
        .output()
        .expect("comprobar rama");
    assert!(
        !branch.status.success(),
        "la rama descartada no debe existir"
    );

    // La segunda limpieza es idempotente: ya no queda nada, pero no rompe.
    let again = post(&app, &format!("/runs/{}/discard", run.id)).await;
    assert_eq!(again.status(), StatusCode::OK);
    let body: Value = read_json(again.into_body()).await;
    assert_eq!(body["worktree_removed"], false);
    assert_eq!(body["branch_deleted"], false);
}

#[tokio::test]
async fn review_rejects_missing_run_and_server_without_repo() {
    let (state, _s, _w) = test_state();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Sin repo", "x.txt", "x\n").await;

    let missing = get(&app, "/runs/00000000-0000-0000-0000-000000000000/diff").await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let no_repo = get(&app, &format!("/runs/{}/diff", run.id)).await;
    assert_eq!(no_repo.status(), StatusCode::BAD_REQUEST);
    let err: Value = read_json(no_repo.into_body()).await;
    assert!(err["error"].as_str().unwrap_or("").contains("repo"));
}

#[tokio::test]
async fn review_merge_rejects_active_run_and_dirty_base() {
    let (state, repo, _data) = test_state_with_repo();
    let slow: Arc<dyn Agent> =
        Arc::new(MockAgent::success("lento").with_delay(Duration::from_secs(5)));
    let app = build_router(state.clone());
    let active = state
        .scheduler
        .submit(
            forja_core::Task::new("Activa", "prompt", Privacy::Open),
            slow.clone(),
        )
        .await
        .expect("encolar activa");

    let busy = post(&app, &format!("/runs/{}/merge", active.id)).await;
    assert_eq!(busy.status(), StatusCode::CONFLICT);
    state
        .scheduler
        .cancel_run(&active.id)
        .await
        .expect("cancelar activa");

    let run = finished_task(&state, "Sucia", "sucia.txt", "contenido\n").await;
    fs::write(repo.path().join("pendiente.txt"), "sin commitear\n").expect("ensuciar base");
    let dirty = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(dirty.status(), StatusCode::BAD_REQUEST);
    let err: Value = read_json(dirty.into_body()).await;
    assert!(
        err["error"]
            .as_str()
            .unwrap_or("")
            .contains("sin commitear"),
        "el error debe explicar el requisito de árbol limpio: {err}"
    );
    // No se tocó el worktree: sigue disponible para descartar o fusionar después.
    assert!(Path::new(run.worktree.as_deref().unwrap()).is_dir());

    fs::remove_file(repo.path().join("pendiente.txt")).expect("limpiar base");
    let merge = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(merge.status(), StatusCode::OK);
}

#[tokio::test]
async fn review_merge_rejects_failed_run_but_discard_cleans_it() {
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let failed = state
        .scheduler
        .submit(
            forja_core::Task::new("Falla", "prompt", Privacy::Open),
            Arc::new(MockAgent::fail("boom")) as Arc<dyn Agent>,
        )
        .await
        .expect("encolar tarea que falla");
    let finished = state
        .scheduler
        .wait_run(&failed.id)
        .await
        .expect("esperar tarea");
    assert_eq!(finished.status, RunStatus::Failed);

    let merge = post(&app, &format!("/runs/{}/merge", failed.id)).await;
    assert_eq!(merge.status(), StatusCode::CONFLICT);
    let err: Value = read_json(merge.into_body()).await;
    assert!(
        err["error"].as_str().unwrap_or("").contains("descartar"),
        "el error debe orientar a descartar: {err}"
    );

    let discard = post(&app, &format!("/runs/{}/discard", failed.id)).await;
    assert_eq!(discard.status(), StatusCode::OK);
    let body: Value = read_json(discard.into_body()).await;
    assert_eq!(body["worktree_removed"], true);
    assert_eq!(body["branch_deleted"], true);
    assert!(
        forja_git::status_clean(repo.path()).expect("status de la base"),
        "descartar no debe tocar la rama base"
    );
}

#[tokio::test]
async fn review_merge_commits_pending_worktree_changes() {
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let run = finished_task(&state, "Pendiente", "base.txt", "base\n").await;
    let worktree = Path::new(run.worktree.as_deref().expect("worktree registrado"));
    fs::write(worktree.join("tarde.txt"), "cambio tardío\n")
        .expect("tocar el worktree tras la tarea");
    assert!(
        !forja_git::status_clean(worktree).expect("status del worktree"),
        "el worktree debe quedar sucio para probar el commit previo"
    );

    let res = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = read_json(res.into_body()).await;
    assert!(
        body["worktree_commit"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "la respuesta debe informar del commit del worktree: {body}"
    );
    assert_eq!(
        fs::read_to_string(repo.path().join("tarde.txt")).expect("cambio tardío fusionado"),
        "cambio tardío\n"
    );
    assert!(!worktree.exists(), "el worktree debe retirarse");
}

#[tokio::test]
async fn review_cancelled_run_can_be_discarded_but_not_merged() {
    let (state, repo, _data) = test_state_with_repo();
    let app = build_router(state.clone());
    let slow: Arc<dyn Agent> =
        Arc::new(MockAgent::success("lento").with_delay(Duration::from_secs(5)));
    let run = state
        .scheduler
        .submit(
            forja_core::Task::new("Cancelada", "prompt", Privacy::Open),
            slow,
        )
        .await
        .expect("encolar tarea lenta");
    let running = wait_until_running(&state, &run.id).await;
    let worktree =
        Path::new(running.worktree.as_deref().expect("worktree registrado")).to_path_buf();
    state
        .scheduler
        .cancel_run(&run.id)
        .await
        .expect("cancelar la tarea");

    let merge = post(&app, &format!("/runs/{}/merge", run.id)).await;
    assert_eq!(
        merge.status(),
        StatusCode::CONFLICT,
        "una ejecución cancelada no se fusiona"
    );

    let discard = post(&app, &format!("/runs/{}/discard", run.id)).await;
    assert_eq!(discard.status(), StatusCode::OK);
    let body: Value = read_json(discard.into_body()).await;
    assert_eq!(body["worktree_removed"], true);
    assert_eq!(body["branch_deleted"], true);
    assert!(!worktree.exists(), "el worktree cancelado debe limpiarse");
    assert!(
        forja_git::status_clean(repo.path()).expect("status de la base"),
        "descartar una cancelada no debe ensuciar la base"
    );
}
