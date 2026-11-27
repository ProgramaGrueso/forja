use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use forja_sched::{Scheduler, SchedulerConfig};
use forja_store::JsonlStore;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::AppState;

use super::*;

fn app(orq: Orchestrator) -> (axum::Router, TempDir, TempDir) {
    let s = TempDir::new().unwrap();
    let w = TempDir::new().unwrap();
    let store = JsonlStore::new(s.path()).unwrap();
    let sched = Scheduler::new(store, SchedulerConfig::new(w.path())).unwrap();
    let state = AppState::new(sched).with_orchestrator(orq);
    (crate::build_router(state), s, w)
}

async fn call(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(method)
        .uri(uri);
    let body = match body {
        Some(v) => {
            b = b.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

#[tokio::test]
async fn chat_without_conversation_id_uses_principal_and_reset_works() {
    let orq = Orchestrator::new(SessionFake::new());
    let (app, _s, _w) = app(orq.clone());
    let (st, v) = call(
        &app,
        Method::POST,
        "/api/orchestrator/chat",
        Some(json!({"message": "hola"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["reply"], "ok");
    assert_eq!(v["conversation_id"], PRINCIPAL);
    assert!(orq.current_session().is_some());
    let (st, _) = call(&app, Method::POST, "/api/orchestrator/reset", None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(orq.current_session().is_none());
}

#[tokio::test]
async fn crud_endpoints_and_conversation_id_in_chat() {
    let (app, _s, _w) = app(Orchestrator::new(SessionFake::new()));
    let (st, v) = call(
        &app,
        Method::POST,
        "/api/orchestrator/conversations",
        Some(json!({"title": "Uno"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let id = v["id"].as_str().unwrap().to_string();
    let (_, v) = call(
        &app,
        Method::POST,
        "/api/orchestrator/chat",
        Some(json!({"message": "hola", "conversation_id": id})),
    )
    .await;
    assert_eq!(v["conversation_id"], id.as_str());
    let (st, v) = call(
        &app,
        Method::GET,
        &format!("/api/orchestrator/conversations/{id}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["titulo"], "Uno");
    assert_eq!(v["historial"].as_array().unwrap().len(), 2);
    let (_, v) = call(&app, Method::GET, "/api/orchestrator/conversations", None).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    // reset por conversación
    let (st, _) = call(
        &app,
        Method::POST,
        "/api/orchestrator/reset",
        Some(json!({"conversation_id": id})),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/orchestrator/conversations/{id}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(
        &app,
        Method::GET,
        &format!("/api/orchestrator/conversations/{id}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &app,
        Method::POST,
        "/api/orchestrator/chat",
        Some(json!({"message": "hola", "conversation_id": id})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn slash_commands_over_http() {
    let orq = Orchestrator::new(SessionFake::new());
    let (app, _s, _w) = app(orq.clone());
    let chat = |msg: &'static str, id: Option<&'static str>| {
        let app = app.clone();
        async move {
            call(
                &app,
                Method::POST,
                "/api/orchestrator/chat",
                Some(json!({"message": msg, "conversation_id": id})),
            )
            .await
        }
    };
    let (st, v) = chat("/new", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["new_conversation"], true);
    let new_id = v["conversation_id"].as_str().unwrap().to_string();
    assert_ne!(new_id, PRINCIPAL);
    assert!(orq.get(&new_id).is_some());

    let (_, v) = chat("/help", None).await;
    let r = v["reply"].as_str().unwrap();
    assert!(r.contains("/rename") && r.contains("/stop") && r.contains("/agents"));
    assert!(r.contains("/model") && r.contains("/effort"), "{r}");

    let (st, _) = chat("/rename", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = chat("/rename Nuevo nombre", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["conversation_id"], PRINCIPAL);
    assert_eq!(orq.get(PRINCIPAL).unwrap().resumen.titulo, "Nuevo nombre");

    let (st, v) = chat("/agents", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["reply"].as_str().unwrap().contains("mock"));
    assert!(!v["agents"].as_array().unwrap().is_empty());

    let (st, v) = chat("/stop", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["stopped"], false);
    assert!(v["reply"].as_str().unwrap().contains("nada en curso"));

    let (st, v) = chat("/agent claude", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["reply"].as_str().unwrap().contains("pronto"));
    // /model y /effort sin argumento solo muestran el valor.
    for cmd in ["/model", "/effort"] {
        let (st, v) = chat(cmd, None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(v["reply"].as_str().unwrap().contains("Ajustes"), "{v}");
    }
    // El doble de pruebas no es una familia conocida: no admite elegir.
    let (st, _) = chat("/model x", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = chat("/loquesea", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // los comandos no entran en el historial ni llaman al backend
    assert_eq!(orq.get(PRINCIPAL).unwrap().historial.len(), 0);
}

#[tokio::test]
async fn new_command_reports_limit() {
    let orq = Orchestrator::new(SessionFake::new());
    for _ in 1..MAX_CONVERSATIONS {
        orq.create(None).unwrap();
    }
    let (app, _s, _w) = app(orq);
    let (st, _) = call(
        &app,
        Method::POST,
        "/api/orchestrator/chat",
        Some(json!({"message": "/new"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
}
