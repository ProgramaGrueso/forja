use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use forja_agent::{Agent, AgyAgent, MockAgent};
use forja_core::{AgentKind, ForjaError, Privacy, Result, RunRecord, Task};
use forja_sched::Scheduler;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct AppState {
    pub scheduler: Scheduler,
    agents: Arc<HashMap<AgentKind, Arc<dyn Agent>>>,
}

impl AppState {
    pub fn new(scheduler: Scheduler) -> Self {
        let mut agents: HashMap<AgentKind, Arc<dyn Agent>> = HashMap::new();
        agents.insert(AgentKind::Mock, Arc::new(MockAgent::default()));
        agents.insert(AgentKind::Agy, Arc::new(AgyAgent::default()));
        Self {
            scheduler,
            agents: Arc::new(agents),
        }
    }

    pub fn with_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        Arc::make_mut(&mut self.agents).insert(agent.kind(), agent);
        self
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
    Router::new()
        .route("/", get(panel_handler))
        .route("/tasks", post(create_task))
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .with_state(state)
}

async fn panel_handler() -> Html<&'static str> {
    Html(PANEL_HTML)
}

pub async fn serve(addr: SocketAddr, state: AppState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}

async fn create_task(
    State(state): State<AppState>,
    Json(req): Json<CreateTaskRequest>,
) -> std::result::Result<(StatusCode, Json<RunRecord>), ApiError> {
    let agent = state.get_agent(req.agent).ok_or_else(|| {
        ApiError(ForjaError::NotConfigured(format!(
            "agente {} no configurado",
            req.agent
        )))
    })?;

    let task = Task::new(req.titulo, req.prompt, req.privacidad);
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

        assert!(body.contains("Forja"));
        assert!(body.contains("Cancelar"));
        assert!(body.contains("/cancel"));
        assert!(!body.contains("http://") && !body.contains("https://"));
    }
}
