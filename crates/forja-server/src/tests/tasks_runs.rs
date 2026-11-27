use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use forja_agent::{Agent, MockAgent};
use forja_core::{AgentKind, Privacy, RunRecord, RunStatus};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{read_json, test_state};
use crate::{build_router, MAX_BODY_BYTES};

#[tokio::test]
async fn api_post_task_and_list_and_get_run() {
    let (state, _store_dir, _wt_dir) = test_state();
    let scheduler = state.scheduler.clone();
    let app = build_router(state);

    let payload = json!({
        "titulo": "Tarea API",
        "prompt": "Ejecuta una prueba"
    });
    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::POST)
        .uri("/tasks")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("construir request POST /tasks");

    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("ejecutar POST /tasks");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let created: RunRecord = read_json(resp.into_body()).await;
    assert_eq!(created.task.titulo, "Tarea API");
    assert_eq!(created.task.privacidad, Privacy::Open);
    assert_eq!(created.agent, AgentKind::Mock);

    let finished = scheduler
        .wait_run(&created.id)
        .await
        .expect("esperar fin de ejecución");
    assert_eq!(finished.status, RunStatus::Done);

    let list_req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::GET)
        .uri("/runs")
        .body(Body::empty())
        .expect("construir request GET /runs");
    let list_resp = app
        .clone()
        .oneshot(list_req)
        .await
        .expect("ejecutar GET /runs");
    assert_eq!(list_resp.status(), StatusCode::OK);

    let runs: Vec<RunRecord> = read_json(list_resp.into_body()).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, created.id);
    assert_eq!(runs[0].status, RunStatus::Done);

    let get_req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::GET)
        .uri(format!("/runs/{}", created.id))
        .body(Body::empty())
        .expect("construir request GET /runs/{id}");
    let get_resp = app.oneshot(get_req).await.expect("ejecutar GET /runs/{id}");
    assert_eq!(get_resp.status(), StatusCode::OK);

    let fetched: RunRecord = read_json(get_resp.into_body()).await;
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.status, RunStatus::Done);
}

#[tokio::test]
async fn api_post_task_policy_denied_returns_403() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state);

    let payload = json!({
        "titulo": "Dato sensible",
        "prompt": "No enviar a agy",
        "privacidad": "sensitive",
        "agent": "agy"
    });
    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::POST)
        .uri("/tasks")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("construir request POST /tasks");

    let resp = app.oneshot(req).await.expect("ejecutar POST /tasks");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body: Value = read_json(resp.into_body()).await;
    let msg = body["error"].as_str().expect("campo error en JSON");
    assert!(msg.contains("agy"));
    assert!(msg.contains("sensitive"));
}

#[tokio::test]
async fn api_cancel_run_marks_cancelled() {
    let (base_state, _store_dir, _wt_dir) = test_state();
    let slow_agent: Arc<dyn Agent> =
        Arc::new(MockAgent::success("lento").with_delay(Duration::from_secs(5)));
    let state = base_state.with_agent(slow_agent);
    let app = build_router(state);

    let payload = json!({
        "titulo": "Tarea lenta",
        "prompt": "Tarda bastante",
        "agent": "mock"
    });
    let create_req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::POST)
        .uri("/tasks")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("construir request POST /tasks");

    let create_resp = app
        .clone()
        .oneshot(create_req)
        .await
        .expect("ejecutar POST /tasks");
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let created: RunRecord = read_json(create_resp.into_body()).await;

    tokio::time::sleep(Duration::from_millis(20)).await;

    let cancel_req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::POST)
        .uri(format!("/runs/{}/cancel", created.id))
        .body(Body::empty())
        .expect("construir request POST /runs/{id}/cancel");
    let cancel_resp = app
        .clone()
        .oneshot(cancel_req)
        .await
        .expect("ejecutar POST /runs/{id}/cancel");
    assert_eq!(cancel_resp.status(), StatusCode::OK);

    let cancelled: RunRecord = read_json(cancel_resp.into_body()).await;
    assert_eq!(cancelled.id, created.id);
    assert_eq!(cancelled.status, RunStatus::Cancelled);

    let get_req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::GET)
        .uri(format!("/runs/{}", created.id))
        .body(Body::empty())
        .expect("construir request GET /runs/{id}");
    let get_resp = app.oneshot(get_req).await.expect("ejecutar GET /runs/{id}");
    assert_eq!(get_resp.status(), StatusCode::OK);
    let fetched: RunRecord = read_json(get_resp.into_body()).await;
    assert_eq!(fetched.status, RunStatus::Cancelled);
}

#[tokio::test]
async fn api_get_missing_run_returns_404() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state);

    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::GET)
        .uri("/runs/no-existe")
        .body(Body::empty())
        .expect("construir request GET /runs/no-existe");
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("ejecutar GET /runs/no-existe");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body: Value = read_json(resp.into_body()).await;
    assert!(body["error"].as_str().is_some());

    let cancel_req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::POST)
        .uri("/runs/no-existe/cancel")
        .body(Body::empty())
        .expect("construir request POST /runs/no-existe/cancel");
    let cancel_resp = app
        .oneshot(cancel_req)
        .await
        .expect("ejecutar POST /runs/no-existe/cancel");
    assert_eq!(cancel_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_post_task_unconfigured_agent_returns_400() {
    let (state, _store_dir, _wt_dir) = test_state();
    let state = state.without_agent(AgentKind::Claude);
    let app = build_router(state);

    let payload = json!({
        "titulo": "Con Claude",
        "prompt": "Usa agente no configurado",
        "agent": "claude"
    });
    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::POST)
        .uri("/tasks")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("construir request POST /tasks");

    let resp = app.oneshot(req).await.expect("ejecutar POST /tasks");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body: Value = read_json(resp.into_body()).await;
    let msg = body["error"].as_str().expect("campo error en JSON");
    assert!(msg.contains("claude"));
}

#[tokio::test]
async fn api_post_task_rejects_empty_title_or_prompt() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state);

    for payload in [
        json!({"titulo": "   ", "prompt": "ok"}),
        json!({"titulo": "ok", "prompt": "  \n "}),
    ] {
        let req = Request::builder()
            .header(header::HOST, "127.0.0.1:7777")
            .method(Method::POST)
            .uri("/tasks")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string()))
            .expect("POST vacío");
        let resp = app.clone().oneshot(req).await.expect("ejecutar POST");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn api_body_limit_rejects_oversized_payload() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state);

    let huge_prompt = "x".repeat(MAX_BODY_BYTES + 4096);
    let payload = json!({ "titulo": "Grande", "prompt": huge_prompt });
    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::POST)
        .uri("/tasks")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("POST gigante");

    let resp = app.oneshot(req).await.expect("ejecutar POST gigante");
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn api_models_returns_catalog_from_injected_runner() {
    use crate::models::{CommandRunner, ModelService};
    use std::time::Duration;

    struct OnlyAgy;
    impl CommandRunner for OnlyAgy {
        fn run(&self, program: &str, _args: &[&str]) -> Result<String, String> {
            if program == "agy" {
                Ok("gemini-4-neon-low\tGemini 4 Neon (Low)\n".to_string())
            } else {
                Err(format!("{program} no está instalado"))
            }
        }
    }

    let (state, _s, _w) = test_state();
    let state = state.with_models(Arc::new(ModelService::new(
        Arc::new(OnlyAgy),
        Duration::from_secs(60),
    )));
    let app = build_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .uri("/api/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let providers = v["providers"].as_array().unwrap();
    let gem = providers
        .iter()
        .find(|p| p["provider"] == "gemini")
        .unwrap();
    assert_eq!(gem["status"], "ok");
    assert_eq!(gem["models"][0]["id"], "gemini-4-neon-low");
    let oc = providers
        .iter()
        .find(|p| p["provider"] == "opencode")
        .unwrap();
    assert_eq!(oc["status"], "unavailable");
}
