pub mod api;
pub mod models;
pub mod quota;
pub mod settings;
pub mod telemetry;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use api::AgentMeta;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use forja_agent::{Agent, AgyAgent, HermesAgent, LocalAgent, MockAgent, OpencodeAgent};
use forja_core::{AgentKind, ForjaError, Privacy, Result, RunRecord, Task};
use forja_sched::Scheduler;
use models::ModelService;
use quota::{default_manual_path, QuotaService};
use serde::{Deserialize, Serialize};
use settings::Settings;
use telemetry::Telemetry;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

pub const MAX_BODY_BYTES: usize = 256 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub scheduler: Scheduler,
    agents: Arc<HashMap<AgentKind, Arc<dyn Agent>>>,
    pub(crate) metas: Arc<Vec<AgentMeta>>,
    pub telemetry: Telemetry,
    pub quota: Arc<QuotaService>,
    pub models: Arc<ModelService>,
    pub settings: Arc<RwLock<Settings>>,
    pub settings_path: Option<PathBuf>,
    pub auth_token: Option<String>,
}

impl AppState {
    pub fn new(scheduler: Scheduler) -> Self {
        let mut agents: HashMap<AgentKind, Arc<dyn Agent>> = HashMap::new();
        agents.insert(AgentKind::Mock, Arc::new(MockAgent::default()));
        agents.insert(AgentKind::Agy, Arc::new(AgyAgent::default()));
        agents.insert(AgentKind::Opencode, Arc::new(OpencodeAgent::default()));
        agents.insert(AgentKind::Hermes, Arc::new(HermesAgent::default()));
        agents.insert(AgentKind::Local, Arc::new(LocalAgent::default()));

        let metas = vec![
            AgentMeta::default_for(AgentKind::Agy, true),
            AgentMeta::default_for(AgentKind::Claude, false),
            AgentMeta::default_for(AgentKind::Hermes, true),
            AgentMeta::default_for(AgentKind::Local, true),
            AgentMeta::default_for(AgentKind::Mock, true),
            AgentMeta::default_for(AgentKind::Opencode, true),
        ];

        let default_quota = default_manual_path();
        let settings = Settings {
            max_concurrency: 2,
            store_dir: ".forja/store".to_string(),
            port: 7777,
            quota_path: default_quota.to_string_lossy().into_owned(),
            hide_mascot: false,
        };

        Self {
            scheduler,
            agents: Arc::new(agents),
            metas: Arc::new(metas),
            telemetry: Telemetry::with_defaults(),
            quota: Arc::new(QuotaService::with_defaults()),
            models: Arc::new(ModelService::with_defaults()),
            settings: Arc::new(RwLock::new(settings)),
            settings_path: None,
            auth_token: None,
        }
    }

    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        let t = token.into();
        self.auth_token = if t.trim().is_empty() { None } else { Some(t) };
        self
    }

    pub fn with_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        let kind = agent.kind();
        Arc::make_mut(&mut self.agents).insert(kind, agent);
        let metas = Arc::make_mut(&mut self.metas);
        if let Some(meta) = metas.iter_mut().find(|m| m.kind == kind) {
            meta.available = true;
        } else {
            metas.push(AgentMeta::default_for(kind, true));
        }
        self
    }

    pub fn with_telemetry(mut self, telemetry: Telemetry) -> Self {
        self.telemetry = telemetry;
        self
    }

    pub fn with_quota(mut self, quota: Arc<QuotaService>) -> Self {
        self.quota = quota;
        self
    }

    pub fn with_models(mut self, models: Arc<ModelService>) -> Self {
        self.models = models;
        self
    }

    pub fn with_settings(self, settings: Settings) -> Self {
        self.quota
            .set_manual_path(PathBuf::from(&settings.quota_path));
        *self.settings.write().unwrap_or_else(|e| e.into_inner()) = settings;
        self
    }

    pub fn with_settings_path(mut self, path: PathBuf) -> Self {
        self.settings_path = Some(path);
        self
    }

    pub(crate) fn on_settings_changed(&self, next: &Settings) {
        self.quota.set_manual_path(PathBuf::from(&next.quota_path));
        let _ = self.scheduler.set_max_concurrency(next.max_concurrency);
    }

    fn get_agent(&self, kind: AgentKind) -> Option<Arc<dyn Agent>> {
        self.agents.get(&kind).cloned()
    }
}

fn default_privacy() -> Privacy {
    Privacy::Open
}

fn default_agent() -> AgentKind {
    AgentKind::Mock
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateTaskRequest {
    pub titulo: String,
    pub prompt: String,
    #[serde(default = "default_privacy")]
    pub privacidad: Privacy,
    #[serde(default = "default_agent")]
    pub agent: AgentKind,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

struct ApiError(ForjaError);

impl From<ForjaError> for ApiError {
    fn from(err: ForjaError) -> Self {
        Self(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            ForjaError::PolicyDenied { .. } => StatusCode::FORBIDDEN,
            ForjaError::InvalidInput(_) | ForjaError::NotConfigured(_) => StatusCode::BAD_REQUEST,
            ForjaError::NotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = ErrorBody {
            error: self.0.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

const PANEL_HTML: &str = include_str!("panel.html");

pub fn build_router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/events", get(events_handler))
        .route("/tasks", post(create_task))
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/api/telemetry", get(api::get_telemetry))
        .route("/api/agents", get(api::agents))
        .route("/api/stats", get(api::stats))
        .route("/api/models", get(api::get_models))
        .route("/api/policy", get(api::policy))
        .route("/api/quota", get(api::get_quota).put(api::put_quota))
        .route(
            "/api/settings",
            get(api::get_settings).put(api::put_settings),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/", get(panel_handler))
        .route("/api/health", get(api::health))
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = state.auth_token.as_deref() else {
        return next.run(req).await;
    };

    let bearer_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .is_some_and(|tok| tok == expected);

    let query_ok = req.uri().query().is_some_and(|q| {
        q.split('&')
            .filter_map(|pair| pair.split_once('='))
            .any(|(k, v)| k == "token" && v == expected)
    });

    if bearer_ok || query_ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "autenticación requerida (Authorization: Bearer <token>)".to_string(),
            }),
        )
            .into_response()
    }
}

async fn panel_handler() -> Html<&'static str> {
    Html(PANEL_HTML)
}

async fn events_handler(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.scheduler.subscribe()).filter_map(|msg| match msg {
        Ok(record) => Event::default().event("run").json_data(record).ok().map(Ok),
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

pub async fn serve(addr: SocketAddr, state: AppState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let sampler = state.telemetry.spawn_sampler();
    let res = axum::serve(listener, build_router(state)).await;
    sampler.abort();
    res?;
    Ok(())
}

async fn create_task(
    State(state): State<AppState>,
    Json(req): Json<CreateTaskRequest>,
) -> std::result::Result<(StatusCode, Json<RunRecord>), ApiError> {
    let titulo = req.titulo.trim();
    let prompt = req.prompt.trim();
    if titulo.is_empty() {
        return Err(ApiError(ForjaError::InvalidInput(
            "el título no puede estar vacío".to_string(),
        )));
    }
    if prompt.is_empty() {
        return Err(ApiError(ForjaError::InvalidInput(
            "el prompt no puede estar vacío".to_string(),
        )));
    }

    let agent = state.get_agent(req.agent).ok_or_else(|| {
        ApiError(ForjaError::NotConfigured(format!(
            "agente {} no configurado",
            req.agent
        )))
    })?;

    let task = Task::new(titulo, prompt, req.privacidad);
    let record = state.scheduler.submit(task, agent).await?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn list_runs(State(state): State<AppState>) -> Json<Vec<RunRecord>> {
    Json(state.scheduler.list_runs().await)
}

async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<RunRecord>, ApiError> {
    let record = state
        .scheduler
        .get_run(&id)
        .await
        .ok_or(ApiError(ForjaError::NotFound(id)))?;
    Ok(Json(record))
}

async fn cancel_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<RunRecord>, ApiError> {
    let record = state.scheduler.cancel_run(&id).await?;
    Ok(Json(record))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{to_bytes, Body};
    use axum::http::{header, Method, Request, StatusCode};
    use forja_agent::{Agent, MockAgent};
    use forja_core::{AgentKind, Privacy, RunRecord, RunStatus};
    use forja_sched::{Scheduler, SchedulerConfig};
    use forja_store::JsonlStore;
    use serde_json::{json, Value};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::{build_router, AppState};

    fn test_state() -> (AppState, TempDir, TempDir) {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");
        let config = SchedulerConfig::new(wt_dir.path());
        let scheduler = Scheduler::new(store, config).expect("crear Scheduler");
        (AppState::new(scheduler), store_dir, wt_dir)
    }

    async fn read_json<T: serde::de::DeserializeOwned>(body: Body) -> T {
        let bytes = to_bytes(body, usize::MAX).await.expect("leer cuerpo HTTP");
        serde_json::from_slice(&bytes).expect("deserializar cuerpo JSON")
    }

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
        let app = build_router(state);

        let payload = json!({
            "titulo": "Con Claude",
            "prompt": "Usa agente no configurado",
            "agent": "claude"
        });
        let req = Request::builder()
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

        let huge_prompt = "x".repeat(super::MAX_BODY_BYTES + 4096);
        let payload = json!({ "titulo": "Grande", "prompt": huge_prompt });
        let req = Request::builder()
            .method(Method::POST)
            .uri("/tasks")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string()))
            .expect("POST gigante");

        let resp = app.oneshot(req).await.expect("ejecutar POST gigante");
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
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
    async fn panel_get_root_returns_html_without_external_urls() {
        let (state, _store_dir, _wt_dir) = test_state();
        let app = build_router(state);

        let req = Request::builder()
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
        assert!(html.contains("<svg id=\"forja-mascot\""));
        assert!(html.contains("prefers-reduced-motion"));
    }

    #[tokio::test]
    async fn panel_views_modal_filters_and_settings_present() {
        let (state, _store_dir, _wt_dir) = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/")
            .body(Body::empty())
            .expect("GET /");
        let resp = app.oneshot(req).await.expect("ejecutar GET /");
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("leer");
        let html = String::from_utf8(bytes.to_vec()).expect("UTF-8");

        for id in [
            "view-resumen",
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
        ] {
            assert!(html.contains(id), "falta elemento #{id} en panel.html");
        }
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

    #[tokio::test]
    async fn events_sse_streams_run_updates() {
        use tokio_stream::StreamExt;

        let (state, _store_dir, _wt_dir) = test_state();
        let scheduler = state.scheduler.clone();
        let app = build_router(state);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/events")
            .body(Body::empty())
            .expect("construir request GET /events");

        let resp = app.oneshot(req).await.expect("ejecutar GET /events");
        assert_eq!(resp.status(), StatusCode::OK);

        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("content-type SSE presente");
        assert!(content_type.contains("text/event-stream"));

        let task = forja_core::Task::new("SSE demo", "prompt sse", Privacy::Open);
        let submitted = scheduler
            .submit(task, Arc::new(MockAgent::success("hecho")))
            .await
            .expect("encolar tarea para SSE");

        let mut stream = resp.into_body().into_data_stream();
        let first_chunk = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("no debe expirar esperando evento SSE")
            .expect("debe llegar al menos un chunk SSE")
            .expect("chunk SSE válido");

        let text = String::from_utf8(first_chunk.to_vec()).expect("UTF-8 válido");
        assert!(
            text.contains("event: run"),
            "esperaba event: run en: {text}"
        );
        assert!(
            text.contains(&submitted.id),
            "esperaba id {} en: {text}",
            submitted.id
        );
    }
}
