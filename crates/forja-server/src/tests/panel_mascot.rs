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

fn assert_in_order(haystack: &str, needles: &[&str], label: &str) {
    let mut cursor = 0usize;
    for needle in needles {
        let rel = haystack[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("falta o está fuera de orden en {label}: {needle}"));
        cursor += rel + needle.len();
    }
}

#[tokio::test]
async fn panel_assembled_html_includes_sections_css_and_js_in_order() {
    let (state, _store_dir, _wt_dir) = test_state();
    let app = build_router(state);

    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .expect("GET /");
    let resp = app.oneshot(req).await.expect("ejecutar GET /");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("leer");
    let html = String::from_utf8(bytes.to_vec()).expect("UTF-8");

    assert!(
        !html.contains("{{CSS}}") && !html.contains("{{JS}}"),
        "quedaron marcadores sin sustituir en el HTML ensamblado"
    );

    let style_start = html.find("<style>").expect("falta <style>");
    let style_end = html.find("</style>").expect("falta </style>");
    let style = &html[style_start..style_end];
    assert_in_order(
        style,
        &[
            ":root {",          // css/base.css
            ".app-shell {",     // css/layout.css
            ".kpi-grid {",      // css/resumen.css
            ".detail-drawer {", // css/ejecuciones.css
            ".grid-cards {",    // css/agentes.css
            ".meter-track {",   // css/telemetria.css
            ".chart-svg {",     // css/telemetria.css
            ".provider-bar {",  // css/cuotas.css
            ".form-field {",    // css/ajustes.css
            ".toast-banner {",  // css/ajustes.css
            ".orq-wrap {",      // css/orquestador.css
            ".term-grid {",     // css/terminales.css
        ],
        "CSS ensamblado",
    );

    let script_start = html.find("<script>").expect("falta <script>");
    let body_html = &html[style_end..script_start];
    assert_in_order(
        body_html,
        &[
            "id=\"view-resumen\"",
            "id=\"view-ejecuciones\"",
            "id=\"view-agentes\"",
            "id=\"view-orquestador\"",
            "id=\"view-terminales\"",
            "id=\"view-telemetria\"",
            "id=\"view-cuotas\"",
            "id=\"view-ajustes\"",
        ],
        "secciones HTML",
    );

    let script_end = html.rfind("</script>").expect("falta </script>");
    let script = &html[script_start..script_end];
    assert_in_order(
        script,
        &[
            "const state = {",             // js/core.js
            "function connectSse()",       // js/core.js
            "function bootPanel()",        // js/core.js
            "function renderSummary()",    // js/resumen.js
            "function renderRunsTable()",  // js/ejecuciones.js
            "function renderAgentsView()", // js/agentes.js
            "function renderTelemetry()",  // js/telemetria.js
            "function renderQuotasView(",  // js/cuotas.js
            "function fillSettingsForm(",  // js/ajustes.js
            "(function orquestador()",     // js/orquestador.js
            "(function terminales()",      // js/terminales.js
            "bootPanel();",                // inicialización final
        ],
        "JS ensamblado",
    );
}
