use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use tower::ServiceExt;

use super::test_state;
use crate::build_router;

#[tokio::test]
async fn panel_get_root_returns_html_without_external_urls() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state);

    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .expect("construir request GET /");

    let resp = app.oneshot(req).await.expect("ejecutar GET /");
    assert_eq!(resp.status(), StatusCode::OK);

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .expect("header content-type presente");
    assert!(content_type.contains("text/html"));

    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("leer cuerpo HTML");
    let body = String::from_utf8(bytes.to_vec()).expect("HTML UTF-8 válido");

    let lower = body.to_ascii_lowercase();
    assert!(body.contains("Forja"));
    assert!(body.contains("Cancelar"));
    assert!(body.contains("/cancel"));
    assert!(!body.contains("http://") && !body.contains("https://"));
    assert!(!lower.contains("<script src"));
    assert!(!lower.contains("<link href"));
}

#[tokio::test]
async fn panel_contains_nav_agents_provider_bar_and_mascot_svg() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state);

    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .expect("construir request GET /");

    let resp = app.oneshot(req).await.expect("ejecutar GET /");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("leer HTML");
    let html = String::from_utf8(bytes.to_vec()).expect("HTML UTF-8");

    // Ítems de navegación y pestañas laterales
    for item in [
        "EJECUCIONES",
        "AGENTES",
        "Resumen",
        "Ejecuciones",
        "Agentes",
        "Telemetría",
        "Cuotas",
        "Ajustes",
    ] {
        assert!(html.contains(item), "falta ítem de navegación: {item}");
    }

    // Agentes base en barra lateral y selects (mock, agy, claude + nuevos)
    for sel in [
        "<option value=\"agy\">",
        "<option value=\"mock\">",
        "<option value=\"claude\">",
        "<option value=\"opencode\">",
        "<option value=\"hermes\">",
        "<option value=\"local\">",
    ] {
        assert!(html.contains(sel), "falta opción de agente en HTML: {sel}");
    }

    // Barra de proveedores fija abajo
    for prov in ["Anthropic", "Copilot", "DeepSeek", "Nous", "Gemini"] {
        assert!(html.contains(prov), "falta proveedor en barra: {prov}");
    }
    assert!(html.contains("id=\"provider-bar\""));

    // Mascota SVG inline y soporte prefers-reduced-motion
    assert!(html.contains("id=\"forja-mascot\""));
    assert!(html.contains("/mascot/idle.webp"));
    assert!(html.contains("prefers-reduced-motion"));
}

#[tokio::test]
async fn mascot_images_are_served_without_auth_and_unknown_is_404() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state.with_auth_token("tk"));
    for (name, expected) in [
        ("idle.webp", StatusCode::OK),
        ("nada.webp", StatusCode::NOT_FOUND),
    ] {
        let req = Request::builder()
            .header(header::HOST, "127.0.0.1:7777")
            .uri(format!("/mascot/{name}"))
            .body(Body::empty())
            .expect("GET mascot");
        let resp = app.clone().oneshot(req).await.expect("respuesta");
        assert_eq!(resp.status(), expected, "{name}");
        if expected == StatusCode::OK {
            assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/webp");
            let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("leer");
            assert_eq!(&bytes[..4], b"RIFF");
        }
    }
}

#[tokio::test]
async fn panel_views_modal_filters_and_settings_present() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state);

    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .expect("GET /");
    let resp = app.oneshot(req).await.expect("ejecutar GET /");
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("leer");
    let html = String::from_utf8(bytes.to_vec()).expect("UTF-8");

    for id in [
        "view-resumen",
        "view-orquestador",
        "orq-call",
        "view-ejecuciones",
        "view-agentes",
        "view-telemetria",
        "view-cuotas",
        "view-ajustes",
        "new-task-modal",
        "nt-policy-banner",
        "filter-status",
        "filter-agent",
        "settings-form",
        "set-hide-mascot",
        "provider-popover",
        "detail-drawer",
        "models-grid",
        "models-refresh",
        "view-terminales",
        "term-grid",
        "term-count",
        "term-cols",
        "orq-conv-list",
        "orq-new",
        "orq-suggest",
        "orq-agent",
        "Agente del orquestador",
        "orq-profile",
        "set-orch-agent",
        "set-orch-profile",
        "set-hidden-profiles",
        "hidden_profiles",
        "set-sensitive-box",
        "set-sensitive-agents",
        "sensitive_agents",
        "Agentes permitidos con privacidad sensitive",
        "orchestrator_agent",
        "FORJA_HIDDEN_PROFILES",
        "set-models-rows",
        "nt-model",
        "nt-effort",
        "orq-model",
        "orq-effort",
        "agent_effort",
        "/effort",
        "esfuerzo",
        "nt-open-term",
        "drawer-term-btn",
        "/api/orchestrator/conversations",
        "/api/orchestrator/profile",
        "/stream?since=",
        "agent_models",
        "orchestrator_profile",
        "drawer-review",
        "drawer-merge-btn",
        "drawer-discard-btn",
        "drawer-review-status",
        "drawer-review-retry",
        "reviewAction",
        "/diff",
    ] {
        assert!(html.contains(id), "falta elemento #{id} en panel.html");
    }
}
