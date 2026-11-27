//! `POST /runs/{id}/retry` y redacción de secretos en las rutas de la API (M10).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use forja_agent::{Agent, MockAgent};
use forja_core::{AgentKind, Privacy, RunRecord, RunStatus};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{get, post, read_json, test_state};
use crate::build_router;

const FAKE_KEY: &str = "sk-live-ABCDEFghijklmnopQRSTUVwxyz0123456789";

async fn post_task(app: &axum::Router, payload: Value) -> axum::response::Response {
    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::POST)
        .uri("/tasks")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("request POST /tasks");
    app.clone().oneshot(req).await.expect("POST /tasks")
}

#[tokio::test]
async fn retry_relaunches_finished_run_with_same_parameters() {
    let (state, _s, _w) = test_state();
    let app = build_router(state.clone());
    let res = post_task(
        &app,
        json!({"titulo": "Repetible", "prompt": "haz algo", "agent": "mock"}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let first: RunRecord = read_json(res.into_body()).await;
    state.scheduler.wait_run(&first.id).await.unwrap();

    let res = post(&app, &format!("/runs/{}/retry", first.id)).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let second: RunRecord = read_json(res.into_body()).await;
    assert_ne!(second.id, first.id);
    assert_eq!(second.task.titulo, "Repetible");
    assert_eq!(second.task.prompt, "haz algo");
    assert_eq!(second.task.privacidad, Privacy::Open);
    assert_eq!(second.agent, AgentKind::Mock);
    let done = state.scheduler.wait_run(&second.id).await.unwrap();
    assert_eq!(done.status, RunStatus::Done);

    let missing = post(&app, "/runs/00000000-0000-0000-0000-000000000000/retry").await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn retry_rejects_active_runs_and_respects_policy() {
    let (base, _s, _w) = test_state();
    let slow: Arc<dyn Agent> =
        Arc::new(MockAgent::success("lento").with_delay(Duration::from_secs(5)));
    let state = base.with_agent(slow);
    let app = build_router(state.clone());
    let res = post_task(
        &app,
        json!({"titulo": "Lenta", "prompt": "p", "agent": "mock"}),
    )
    .await;
    let run: RunRecord = read_json(res.into_body()).await;
    let busy = post(&app, &format!("/runs/{}/retry", run.id)).await;
    assert_eq!(busy.status(), StatusCode::CONFLICT);
    state.scheduler.cancel_run(&run.id).await.unwrap();
    let again = post(&app, &format!("/runs/{}/retry", run.id)).await;
    assert_eq!(
        again.status(),
        StatusCode::CREATED,
        "cancelada sí se relanza"
    );
    let relaunched: RunRecord = read_json(again.into_body()).await;
    state.scheduler.cancel_run(&relaunched.id).await.unwrap();

    // Misma política que POST /tasks: si ahora mock no puede con sensitive, 403.
    state
        .scheduler
        .set_sensitive_agents(vec![AgentKind::Mock])
        .unwrap();
    let res = post_task(
        &app,
        json!({"titulo": "S", "prompt": "p", "agent": "mock", "privacidad": "sensitive"}),
    )
    .await;
    let sensitive: RunRecord = read_json(res.into_body()).await;
    state.scheduler.cancel_run(&sensitive.id).await.unwrap();
    state
        .scheduler
        .set_sensitive_agents(vec![AgentKind::Claude])
        .unwrap();
    let denied = post(&app, &format!("/runs/{}/retry", sensitive.id)).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn secrets_never_leave_the_api_and_redacted_prompts_are_not_retried() {
    let (state, store_dir, _w) = test_state();
    let app = build_router(state.clone());
    let res = post_task(
        &app,
        json!({"titulo": "Clave", "prompt": format!("usa {FAKE_KEY}"), "agent": "mock"}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let run: RunRecord = read_json(res.into_body()).await;
    state.scheduler.wait_run(&run.id).await.unwrap();

    for uri in [format!("/runs/{}", run.id), "/runs".to_string()] {
        let res = get(&app, &uri).await;
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(FAKE_KEY),
            "{uri} filtra la clave"
        );
    }
    let jsonl = std::fs::read_to_string(store_dir.path().join("runs.jsonl")).unwrap();
    assert!(!jsonl.contains(FAKE_KEY));

    let res = post(&app, &format!("/runs/{}/retry", run.id)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let err: Value = read_json(res.into_body()).await;
    assert!(err["error"].as_str().unwrap_or("").contains("redactados"));
}

#[tokio::test]
async fn oversized_prompt_gets_clear_400() {
    let (state, _s, _w) = test_state();
    let app = build_router(state);
    let prompt = "x".repeat(forja_core::limits::MAX_PROMPT_BYTES + 10);
    let res = post_task(
        &app,
        json!({"titulo": "G", "prompt": prompt, "agent": "mock"}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err: Value = read_json(res.into_body()).await;
    assert!(err["error"].as_str().unwrap_or("").contains("KiB"), "{err}");
}
