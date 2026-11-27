use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{read_json, test_state};
use crate::build_router;

fn guarded_request(
    method: Method,
    uri: &str,
    host: Option<&str>,
    origin: Option<&str>,
    body: Body,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(host) = host {
        builder = builder.header(header::HOST, host);
    }
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder = builder.header(header::CONTENT_TYPE, "application/json");
    builder.body(body).expect("construir request con guardas")
}

#[tokio::test]
async fn api_bearer_auth_protects_routes_when_enabled() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state.with_auth_token("secreto-local-123"));

    // /api/health sigue accesible sin token
    let health_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(Method::GET)
                .uri("/api/health")
                .body(Body::empty())
                .expect("GET health"),
        )
        .await
        .expect("health");
    assert_eq!(health_resp.status(), StatusCode::OK);

    // /runs sin token -> 401
    let unauth = app
        .clone()
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(Method::GET)
                .uri("/runs")
                .body(Body::empty())
                .expect("GET runs"),
        )
        .await
        .expect("runs unauth");
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    // /runs con Bearer correcto -> 200
    let auth_ok = app
        .clone()
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(Method::GET)
                .uri("/runs")
                .header(header::AUTHORIZATION, "Bearer secreto-local-123")
                .body(Body::empty())
                .expect("GET runs auth"),
        )
        .await
        .expect("runs auth");
    assert_eq!(auth_ok.status(), StatusCode::OK);

    // /events?token=... -> 200 (soporte EventSource)
    let sse_ok = app
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(Method::GET)
                .uri("/events?token=secreto-local-123")
                .body(Body::empty())
                .expect("GET events query token"),
        )
        .await
        .expect("events auth");
    assert_eq!(sse_ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn security_rejects_foreign_and_malformed_hosts() {
    let (state, _s, _w) = test_state();
    let app = build_router(state);

    for host in [
        "evil.example:7777",
        "localhost.evil.example:7777",
        "127.0.0.1:9999",
        "0.0.0.0:7777",
    ] {
        let res = app
            .clone()
            .oneshot(guarded_request(
                Method::GET,
                "/api/health",
                Some(host),
                None,
                Body::empty(),
            ))
            .await
            .expect("respuesta health");
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "host {host}");
        let body: Value = read_json(res.into_body()).await;
        assert!(body["error"].as_str().is_some(), "cuerpo de error JSON");
    }

    // Sin Host no se puede verificar el destino: también se rechaza.
    let missing = app
        .clone()
        .oneshot(guarded_request(
            Method::GET,
            "/api/health",
            None,
            None,
            Body::empty(),
        ))
        .await
        .expect("respuesta sin host");
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    // El panel y la mascota (rutas públicas) pasan por la misma guarda.
    let panel = app
        .clone()
        .oneshot(guarded_request(
            Method::GET,
            "/",
            Some("evil.example:7777"),
            None,
            Body::empty(),
        ))
        .await
        .expect("respuesta panel");
    assert_eq!(panel.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn security_accepts_all_local_host_forms() {
    let (state, _s, _w) = test_state();
    let app = build_router(state);

    for host in ["localhost:7777", "127.0.0.1:7777", "[::1]:7777"] {
        let res = app
            .clone()
            .oneshot(guarded_request(
                Method::GET,
                "/api/health",
                Some(host),
                None,
                Body::empty(),
            ))
            .await
            .expect("respuesta health");
        assert_eq!(res.status(), StatusCode::OK, "host {host}");
    }
}

#[tokio::test]
async fn security_rejects_invalid_origin_on_mutating_and_read_routes() {
    let (state, _s, _w) = test_state();
    let scheduler = state.scheduler.clone();
    let app = build_router(state);

    let payload = json!({ "titulo": "CSRF", "prompt": "no debe crearse" }).to_string();
    for origin in [
        "http://evil.example",
        "http://evil.example:7777",
        "http://localhost:9999",
        "null",
        "https://localhost:7777",
    ] {
        let res = app
            .clone()
            .oneshot(guarded_request(
                Method::POST,
                "/tasks",
                Some("127.0.0.1:7777"),
                Some(origin),
                Body::from(payload.clone()),
            ))
            .await
            .expect("respuesta POST /tasks");
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "origin {origin}");
    }
    // Ninguna petición rechazada debe haber llegado al planificador.
    assert!(scheduler.list_runs().await.is_empty());

    // Lecturas cross-origin también se bloquean (fuga de datos).
    let read = app
        .clone()
        .oneshot(guarded_request(
            Method::GET,
            "/runs",
            Some("127.0.0.1:7777"),
            Some("http://evil.example:7777"),
            Body::empty(),
        ))
        .await
        .expect("respuesta GET /runs");
    assert_eq!(read.status(), StatusCode::FORBIDDEN);

    // SSE /events (origen inválido) también.
    let events = app
        .clone()
        .oneshot(guarded_request(
            Method::GET,
            "/events",
            Some("127.0.0.1:7777"),
            Some("http://evil.example:7777"),
            Body::empty(),
        ))
        .await
        .expect("respuesta GET /events");
    assert_eq!(events.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn security_allows_valid_local_origin_and_cli_requests() {
    let (state, _s, _w) = test_state();
    let scheduler = state.scheduler.clone();
    let app = build_router(state);

    let body = json!({ "titulo": "Local", "prompt": "permitida" }).to_string();
    let res = app
        .clone()
        .oneshot(guarded_request(
            Method::POST,
            "/tasks",
            Some("127.0.0.1:7777"),
            Some("http://localhost:7777"),
            Body::from(body.clone()),
        ))
        .await
        .expect("POST /tasks con origin local");
    assert_eq!(res.status(), StatusCode::CREATED);

    // Sin Origin (curl/agentes locales) también se permite.
    let res = app
        .clone()
        .oneshot(guarded_request(
            Method::POST,
            "/tasks",
            Some("localhost:7777"),
            None,
            Body::from(body),
        ))
        .await
        .expect("POST /tasks sin origin");
    assert_eq!(res.status(), StatusCode::CREATED);

    assert_eq!(scheduler.list_runs().await.len(), 2);
}

#[tokio::test]
async fn security_rejects_cross_site_fetch_metadata() {
    let (state, _s, _w) = test_state();
    let app = build_router(state);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/health")
        .header(header::HOST, "127.0.0.1:7777")
        .header("sec-fetch-site", "cross-site")
        .body(Body::empty())
        .expect("request cross-site");
    let res = app.oneshot(req).await.expect("respuesta");
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}
