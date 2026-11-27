//! Rutas `/api/*` del dashboard: agentes, estadísticas, política, salud y ajustes.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use forja_core::{allowed_agents_with, AgentKind, Privacy, RunRecord, RunStatus};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::quota::{ManualQuotaFile, ProviderQuota};
use crate::settings::{save_settings, Settings};
use crate::telemetry::TelemetrySnapshot;
use crate::AppState;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const HOUR_MS: u64 = 3_600_000;
const TIMELINE_BUCKETS: u64 = 24;
const RECENT_LIMIT: usize = 8;

/// Metadatos de un agente registrado (o conocido pero no configurado).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentMeta {
    pub kind: AgentKind,
    pub provider: String,
    pub model: String,
    /// Esfuerzo de razonamiento efectivo (ajustes); None = no configurado.
    pub effort: Option<String>,
    pub available: bool,
}

impl AgentMeta {
    pub fn default_for(kind: AgentKind, available: bool) -> Self {
        let (provider, model) = match kind {
            AgentKind::Mock => ("Local", "mock"),
            AgentKind::Agy => ("Gemini", forja_agent::agy::DEFAULT_MODEL),
            AgentKind::Claude => ("Anthropic", forja_agent::claude::DEFAULT_MODEL),
            AgentKind::Opencode => ("OpenCode", forja_agent::opencode::DEFAULT_MODEL),
            AgentKind::Hermes => ("Hermes", forja_agent::hermes::DEFAULT_PROFILE),
            AgentKind::Local => ("Local", forja_agent::local::DEFAULT_MODEL),
        };
        Self {
            kind,
            provider: provider.to_string(),
            model: model.to_string(),
            effort: None,
            available,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentView {
    pub kind: AgentKind,
    pub name: String,
    pub provider: String,
    pub model: String,
    /// Esfuerzo de razonamiento efectivo (None = no configurado: no se pasa flag).
    pub effort: Option<String>,
    /// Niveles de esfuerzo de la familia (vacía = no soporta). En opencode son solo
    /// sugerencias: acepta una variante libre `[a-z0-9_-]{1,20}`.
    pub effort_levels: Vec<String>,
    /// El modelo se puede elegir (ajustes / tarea) en este agente.
    pub model_editable: bool,
    pub available: bool,
    /// `idle` | `busy` | `error` | `unconfigured`
    pub state: String,
    pub active: usize,
    pub queued: usize,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub success_rate: Option<f64>,
    pub avg_duration_ms: Option<u64>,
    pub last_error: Option<String>,
}

fn duration_ms(run: &RunRecord) -> u64 {
    run.updated_at_ms.saturating_sub(run.created_at_ms)
}

fn is_finished(status: RunStatus) -> bool {
    matches!(status, RunStatus::Done | RunStatus::Failed)
}

pub fn build_agent_views(metas: &[AgentMeta], runs: &[RunRecord]) -> Vec<AgentView> {
    let mut views: Vec<AgentView> = metas
        .iter()
        .map(|meta| {
            let own: Vec<&RunRecord> = runs.iter().filter(|r| r.agent == meta.kind).collect();
            let count = |s: RunStatus| own.iter().filter(|r| r.status == s).count();
            let active = count(RunStatus::Running);
            let queued = count(RunStatus::Pending);
            let done = count(RunStatus::Done);
            let failed = count(RunStatus::Failed);
            let cancelled = count(RunStatus::Cancelled);

            let finished: Vec<&&RunRecord> = own.iter().filter(|r| is_finished(r.status)).collect();
            let success_rate = if finished.is_empty() {
                None
            } else {
                Some(done as f64 / finished.len() as f64)
            };
            let avg_duration_ms = if finished.is_empty() {
                None
            } else {
                let sum: u64 = finished.iter().map(|r| duration_ms(r)).sum();
                Some(sum / finished.len() as u64)
            };
            let last_finished = finished.iter().max_by_key(|r| r.updated_at_ms);
            let last_error = last_finished
                .filter(|r| r.status == RunStatus::Failed)
                .and_then(|r| r.error.clone());

            let state = if !meta.available {
                "unconfigured"
            } else if active > 0 {
                "busy"
            } else if last_error.is_some()
                || last_finished.is_some_and(|r| r.status == RunStatus::Failed)
            {
                "error"
            } else {
                "idle"
            };

            AgentView {
                kind: meta.kind,
                name: meta.kind.to_string(),
                provider: meta.provider.clone(),
                model: meta.model.clone(),
                effort: meta.effort.clone(),
                effort_levels: forja_agent::effort_levels(meta.kind)
                    .iter()
                    .map(|l| l.to_string())
                    .collect(),
                model_editable: forja_agent::MODEL_AGENTS.contains(&meta.kind),
                available: meta.available,
                state: state.to_string(),
                active,
                queued,
                total: own.len(),
                done,
                failed,
                cancelled,
                success_rate,
                avg_duration_ms,
                last_error,
            }
        })
        .collect();
    views.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.name.cmp(&b.name))
    });
    views
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineBucket {
    pub start_ms: u64,
    pub done: usize,
    pub failed: usize,
    pub other: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    pub version: &'static str,
    pub now_ms: u64,
    pub total: usize,
    pub active: usize,
    pub queued: usize,
    pub done_24h: usize,
    pub failed_24h: usize,
    pub cancelled_24h: usize,
    pub timeline: Vec<TimelineBucket>,
    pub recent: Vec<RunRecord>,
}

pub fn build_stats(runs: &[RunRecord], now_ms: u64) -> Stats {
    let window_start = now_ms.saturating_sub(TIMELINE_BUCKETS * HOUR_MS);
    let current_hour = now_ms - now_ms % HOUR_MS;
    let first_bucket = current_hour.saturating_sub((TIMELINE_BUCKETS - 1) * HOUR_MS);

    let mut timeline: Vec<TimelineBucket> = (0..TIMELINE_BUCKETS)
        .map(|i| TimelineBucket {
            start_ms: first_bucket + i * HOUR_MS,
            done: 0,
            failed: 0,
            other: 0,
        })
        .collect();

    let mut done_24h = 0;
    let mut failed_24h = 0;
    let mut cancelled_24h = 0;
    for run in runs {
        if run.updated_at_ms >= window_start {
            match run.status {
                RunStatus::Done => done_24h += 1,
                RunStatus::Failed => failed_24h += 1,
                RunStatus::Cancelled => cancelled_24h += 1,
                _ => {}
            }
        }
        if run.created_at_ms >= first_bucket {
            let idx = ((run.created_at_ms - first_bucket) / HOUR_MS) as usize;
            if let Some(bucket) = timeline.get_mut(idx) {
                match run.status {
                    RunStatus::Done => bucket.done += 1,
                    RunStatus::Failed => bucket.failed += 1,
                    _ => bucket.other += 1,
                }
            }
        }
    }

    let mut recent: Vec<RunRecord> = runs.to_vec();
    recent.sort_by_key(|a| std::cmp::Reverse(a.updated_at_ms));
    recent.truncate(RECENT_LIMIT);

    Stats {
        version: VERSION,
        now_ms,
        total: runs.len(),
        active: runs
            .iter()
            .filter(|r| r.status == RunStatus::Running)
            .count(),
        queued: runs
            .iter()
            .filter(|r| r.status == RunStatus::Pending)
            .count(),
        done_24h,
        failed_24h,
        cancelled_24h,
        timeline,
        recent,
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// Metadatos con el modelo y esfuerzo efectivos (ajustes > default) de cada agente.
pub fn effective_metas(
    metas: &[AgentMeta],
    agent_models: &std::collections::BTreeMap<String, String>,
    agent_effort: &std::collections::BTreeMap<String, String>,
) -> Vec<AgentMeta> {
    metas
        .iter()
        .map(|meta| {
            let mut meta = meta.clone();
            if forja_agent::MODEL_AGENTS.contains(&meta.kind) {
                if let Some(m) = agent_models
                    .get(&meta.kind.to_string())
                    .map(|m| m.trim())
                    .filter(|m| !m.is_empty())
                {
                    meta.model = m.to_string();
                }
            }
            meta.effort = forja_agent::resolve_effort(meta.kind, None, agent_effort);
            meta
        })
        .collect()
}

pub async fn agents(State(state): State<AppState>) -> Json<Vec<AgentView>> {
    let runs = state.scheduler.list_runs().await;
    let (models, efforts) = {
        let g = state.settings.read().unwrap_or_else(|e| e.into_inner());
        (g.agent_models.clone(), g.agent_effort.clone())
    };
    Json(build_agent_views(
        &effective_metas(&state.metas, &models, &efforts),
        &runs,
    ))
}

/// Agente y perfil efectivos del orquestador, su origen y lo elegible.
///
/// `allowed` se conserva por compatibilidad y es igual a `profiles` (los perfiles
/// de Hermes descubiertos, sin los ocultos).
pub async fn orchestrator_profile(State(state): State<AppState>) -> Json<serde_json::Value> {
    let (configured, configured_agent, agent_models, agent_effort) = {
        let g = state.settings.read().unwrap_or_else(|e| e.into_inner());
        (
            g.orchestrator_profile.clone(),
            g.orchestrator_agent.clone(),
            g.agent_models.clone(),
            g.agent_effort.clone(),
        )
    };
    let env = std::env::var(crate::orchestrator::PROFILE_ENV).ok();
    let source = if configured.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        "settings"
    } else if env.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        "env"
    } else {
        "default"
    };
    let orq = state.orchestrator.clone();
    let profiles = orq.profiles();
    let profile = orq.profile().unwrap_or_else(|| {
        crate::orchestrator::resolve_profile(configured.as_deref(), env.as_deref())
    });
    // Modelo y esfuerzo que usa el orquestador sin override de conversación, y qué
    // se puede elegir en el composer del chat.
    let orq_agent = orq.agent();
    let orq_kind = orq_agent.parse::<forja_core::AgentKind>().ok();
    let model_options: &[&str] = match orq_kind {
        Some(forja_core::AgentKind::Claude) => &["sonnet", "opus", "haiku"],
        _ => &[],
    };
    let model = orq_kind.and_then(|k| forja_agent::resolve_model(k, None, &agent_models));
    let effort = orq_kind.and_then(|k| forja_agent::resolve_effort(k, None, &agent_effort));
    let effort_levels: Vec<&str> = orq_kind
        .map(|k| forja_agent::effort_levels(k).to_vec())
        .unwrap_or_default();
    Json(json!({
        "model": model,
        "effort": effort,
        "effort_levels": effort_levels,
        "model_options": model_options,
        "model_editable": orq_kind.is_some_and(|k| forja_agent::MODEL_AGENTS.contains(&k)),
        "profile": profile,
        "source": source,
        "allowed": profiles,
        "profiles": profiles,
        "agent": orq.agent(),
        "agent_source": orq.agent_source(configured_agent.as_deref()),
        "agents_available": orq.agents_available(),
        "agents_supported": crate::orchestrator::ORCHESTRATOR_AGENTS,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ModelsQuery {
    #[serde(default)]
    pub refresh: bool,
}

/// Modelos detectados en la máquina (`?refresh=true` ignora la caché).
pub async fn get_models(State(state): State<AppState>, Query(q): Query<ModelsQuery>) -> Response {
    let models = state.models.clone();
    let hidden = state.profile_rules.hidden();
    match tokio::task::spawn_blocking(move || models.catalog(q.refresh, &hidden)).await {
        Ok(catalog) => Json(catalog).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("fallo detectando modelos: {err}") })),
        )
            .into_response(),
    }
}

pub async fn stats(State(state): State<AppState>) -> Json<Stats> {
    let runs = state.scheduler.list_runs().await;
    Json(build_stats(&runs, now_ms()))
}

pub async fn policy(State(state): State<AppState>) -> Json<serde_json::Value> {
    let sensitive = state
        .settings
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .sensitive_agents
        .clone();
    Json(json!({
        "open": allowed_agents_with(Privacy::Open, &sensitive),
        "sensitive": allowed_agents_with(Privacy::Sensitive, &sensitive),
    }))
}

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}

pub async fn get_settings(State(state): State<AppState>) -> Json<Settings> {
    let settings = state
        .settings
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Json(settings)
}

pub async fn put_settings(
    State(state): State<AppState>,
    body: Result<Json<Settings>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(mut next) = match body {
        Ok(json) => json,
        Err(err) => return error_response(StatusCode::BAD_REQUEST, err.body_text()),
    };
    next.normalize();
    if let Err(msg) = next.validate() {
        return error_response(StatusCode::BAD_REQUEST, msg);
    }
    if let Err(msg) = state.orchestrator.check_settings(&next) {
        return error_response(StatusCode::BAD_REQUEST, msg);
    }

    let previous = state
        .settings
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let restart_required = next.restart_required(&previous);

    let persisted = match &state.settings_path {
        Some(path) => {
            let path = path.clone();
            let to_save = next.clone();
            match tokio::task::spawn_blocking(move || save_settings(&path, &to_save)).await {
                Ok(Ok(())) => true,
                Ok(Err(msg)) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, msg),
                Err(err) => {
                    return error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                }
            }
        }
        None => false,
    };

    state.on_settings_changed(&next);
    *state.settings.write().unwrap_or_else(|e| e.into_inner()) = next.clone();

    Json(json!({
        "settings": next,
        "restart_required": restart_required,
        "persisted": persisted,
    }))
    .into_response()
}

pub async fn get_telemetry(State(state): State<AppState>) -> Json<TelemetrySnapshot> {
    let mut snap = state.telemetry.snapshot();
    if snap.latest.is_none() {
        let telemetry = state.telemetry.clone();
        let _ = tokio::task::spawn_blocking(move || telemetry.sample_once()).await;
        snap = state.telemetry.snapshot();
    }
    Json(snap)
}

#[derive(Debug, Serialize)]
pub struct QuotaResponse {
    pub providers: Vec<ProviderQuota>,
    pub manual: ManualQuotaFile,
    pub quota_path: String,
    pub manual_error: Option<String>,
}

pub async fn get_quota(State(state): State<AppState>) -> Response {
    let quota = state.quota.clone();
    let res = tokio::task::spawn_blocking(move || {
        let providers = quota.get_all();
        let quota_path = quota.manual_path().to_string_lossy().into_owned();
        let (manual, manual_error) = match quota.manual() {
            Ok(m) => (m, None),
            Err(err) => (ManualQuotaFile::default(), Some(err)),
        };
        QuotaResponse {
            providers,
            manual,
            quota_path,
            manual_error,
        }
    })
    .await;

    match res {
        Ok(payload) => Json(payload).into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub async fn put_quota(
    State(state): State<AppState>,
    body: Result<Json<ManualQuotaFile>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(file) = match body {
        Ok(json) => json,
        Err(err) => return error_response(StatusCode::BAD_REQUEST, err.body_text()),
    };

    let quota = state.quota.clone();
    let res = tokio::task::spawn_blocking(move || {
        quota.put_manual(file)?;
        let providers = quota.get_all();
        let quota_path = quota.manual_path().to_string_lossy().into_owned();
        let manual = quota.manual().unwrap_or_default();
        Ok::<QuotaResponse, String>(QuotaResponse {
            providers,
            manual,
            quota_path,
            manual_error: None,
        })
    })
    .await;

    match res {
        Ok(Ok(payload)) => Json(payload).into_response(),
        Ok(Err(msg)) => error_response(StatusCode::BAD_REQUEST, msg),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Method, Request, StatusCode};
    use forja_core::{RunRecord, RunStatus, Task};
    use forja_sched::{Scheduler, SchedulerConfig};
    use forja_store::JsonlStore;
    use serde_json::{json, Value};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::{build_router, AppState};

    fn state() -> (AppState, TempDir, TempDir) {
        let store_dir = TempDir::new().expect("store tempdir");
        let wt_dir = TempDir::new().expect("wt tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("store");
        let scheduler =
            Scheduler::new(store, SchedulerConfig::new(wt_dir.path())).expect("scheduler");
        (AppState::new(scheduler), store_dir, wt_dir)
    }

    async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .header(header::HOST, "127.0.0.1:7777")
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("respuesta");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("cuerpo");
        (status, serde_json::from_slice(&bytes).expect("json"))
    }

    fn run(agent: AgentKind, status: RunStatus, created: u64, updated: u64) -> RunRecord {
        RunRecord {
            id: format!("{agent}-{created}"),
            task: Task::new("t", "p", Privacy::Open),
            agent,
            status,
            branch: None,
            worktree: None,
            attempts: 1,
            model: None,
            effort: None,
            output: None,
            error: (status == RunStatus::Failed).then(|| "fallo".to_string()),
            created_at_ms: created,
            updated_at_ms: updated,
        }
    }

    #[test]
    fn api_agent_views_compute_state_and_metrics() {
        let metas = vec![
            AgentMeta::default_for(AgentKind::Mock, true),
            AgentMeta::default_for(AgentKind::Agy, true),
            AgentMeta::default_for(AgentKind::Claude, false),
        ];
        let runs = vec![
            run(AgentKind::Mock, RunStatus::Done, 0, 1000),
            run(AgentKind::Mock, RunStatus::Failed, 2000, 5000),
            run(AgentKind::Agy, RunStatus::Running, 100, 100),
        ];
        let views = build_agent_views(&metas, &runs);
        let by = |k: AgentKind| views.iter().find(|v| v.kind == k).expect("vista");

        let mock = by(AgentKind::Mock);
        assert_eq!(mock.state, "error");
        assert_eq!(mock.total, 2);
        assert_eq!(mock.success_rate, Some(0.5));
        assert_eq!(mock.avg_duration_ms, Some(2000));

        let agy = by(AgentKind::Agy);
        assert_eq!(agy.state, "busy");
        assert_eq!(agy.active, 1);
        assert_eq!(agy.success_rate, None);

        assert_eq!(by(AgentKind::Claude).state, "unconfigured");
        // Ordenados por proveedor: Anthropic, Gemini, Local.
        let providers: Vec<&str> = views.iter().map(|v| v.provider.as_str()).collect();
        assert_eq!(providers, vec!["Anthropic", "Gemini", "Local"]);
    }

    #[test]
    fn api_stats_buckets_and_counters() {
        let now = 100 * HOUR_MS + 30 * 60_000;
        let runs = vec![
            run(AgentKind::Mock, RunStatus::Done, now - 10, now - 5),
            run(
                AgentKind::Mock,
                RunStatus::Failed,
                now - 2 * HOUR_MS,
                now - 2 * HOUR_MS,
            ),
            run(
                AgentKind::Mock,
                RunStatus::Done,
                now - 48 * HOUR_MS,
                now - 48 * HOUR_MS,
            ),
            run(AgentKind::Agy, RunStatus::Pending, now - 1, now - 1),
        ];
        let stats = build_stats(&runs, now);
        assert_eq!(stats.total, 4);
        assert_eq!(stats.queued, 1);
        assert_eq!(stats.done_24h, 1);
        assert_eq!(stats.failed_24h, 1);
        assert_eq!(stats.timeline.len(), 24);
        let last = stats.timeline.last().expect("último bucket");
        assert_eq!((last.done, last.failed, last.other), (1, 0, 1));
        assert_eq!(stats.timeline[21].failed, 1);
        assert_eq!(stats.recent.len(), 4);
        assert!(stats.recent[0].updated_at_ms >= stats.recent[1].updated_at_ms);
    }

    #[tokio::test]
    async fn api_agents_stats_policy_health_routes() {
        let (state, _s, _w) = state();
        let app = build_router(state);

        let (status, agents) = get_json(app.clone(), "/api/agents").await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = agents
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|a| a["name"].as_str())
            .collect();
        assert!(names.contains(&"agy") && names.contains(&"mock") && names.contains(&"claude"));

        let (status, stats) = get_json(app.clone(), "/api/stats").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(stats["total"], 0);
        assert_eq!(stats["timeline"].as_array().map(Vec::len), Some(24));

        let (status, policy) = get_json(app.clone(), "/api/policy").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(policy["sensitive"], json!(["claude"]));
        assert_eq!(
            policy["open"],
            json!(["agy", "mock", "claude", "opencode", "hermes", "local"])
        );

        let (status, health) = get_json(app, "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(health["version"], VERSION);
    }

    #[tokio::test]
    async fn api_agents_lists_all_six_available() {
        let (state, _s, _w) = state();
        let app = build_router(state);

        let (status, agents) = get_json(app, "/api/agents").await;
        assert_eq!(status, StatusCode::OK);
        let arr = agents.as_array().expect("array de agentes");
        assert_eq!(arr.len(), 6, "deben listar los 6 agentes");

        let by_name = |name: &str| {
            arr.iter()
                .find(|a| a["name"] == name)
                .unwrap_or_else(|| panic!("falta agente {name}"))
        };

        for name in ["mock", "agy", "claude", "opencode", "hermes", "local"] {
            assert_eq!(
                by_name(name)["available"],
                true,
                "{name} debe estar available"
            );
        }
        assert_eq!(by_name("claude")["provider"], "Anthropic");
        assert_eq!(by_name("claude")["model"], "sonnet");

        assert_eq!(by_name("opencode")["provider"], "OpenCode");
        assert_eq!(
            by_name("opencode")["model"],
            "opencode-go/deepseek-v4.1-flash"
        );
        assert_eq!(by_name("hermes")["provider"], "Hermes");
        assert_eq!(by_name("hermes")["model"], "default");
        assert_eq!(by_name("local")["provider"], "Local");
        assert_eq!(by_name("local")["model"], "moe");
    }

    #[tokio::test]
    async fn api_settings_get_put_validation_and_persistence() {
        let (state, store_dir, _w) = state();
        let path = store_dir.path().join("settings.json");
        let app = build_router(state.with_settings_path(path.clone()));

        let (status, current) = get_json(app.clone(), "/api/settings").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(current["max_concurrency"], 2);

        let mut next = current.clone();
        next["max_concurrency"] = json!(4);
        next["port"] = json!(7799);
        next["hide_mascot"] = json!(true);
        let put = |body: Value| {
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .method(Method::PUT)
                .uri("/api/settings")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .expect("request PUT")
        };

        let resp = app.clone().oneshot(put(next.clone())).await.expect("PUT");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("cuerpo");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["restart_required"], json!(["port"]));
        assert_eq!(body["persisted"], true);
        assert!(path.exists());

        let (_, after) = get_json(app.clone(), "/api/settings").await;
        assert_eq!(after["hide_mascot"], true);

        let mut bad = next.clone();
        bad["max_concurrency"] = json!(0);
        let resp = app.clone().oneshot(put(bad)).await.expect("PUT inválido");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let req = Request::builder()
            .header(header::HOST, "127.0.0.1:7777")
            .method(Method::PUT)
            .uri("/api/settings")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{roto"))
            .expect("request roto");
        let resp = app.oneshot(req).await.expect("PUT roto");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    async fn put_settings_json(app: axum::Router, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .header(header::HOST, "127.0.0.1:7777")
            .method(Method::PUT)
            .uri("/api/settings")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request PUT");
        let resp = app.oneshot(req).await.expect("PUT");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("cuerpo");
        (status, serde_json::from_slice(&bytes).expect("json"))
    }

    async fn post_task(app: axum::Router, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .header(header::HOST, "127.0.0.1:7777")
            .method(Method::POST)
            .uri("/tasks")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request POST");
        let resp = app.oneshot(req).await.expect("POST");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("cuerpo");
        (status, serde_json::from_slice(&bytes).expect("json"))
    }

    #[tokio::test]
    async fn api_agent_models_in_settings_shown_in_agents_and_validated() {
        let (state, _s, _w) = state();
        let app = build_router(state);
        let (_, current) = get_json(app.clone(), "/api/settings").await;

        let mut next = current.clone();
        next["agent_models"] = json!({"claude": "opus", "local": " qwen "});
        let (status, body) = put_settings_json(app.clone(), next.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["settings"]["agent_models"]["local"], "qwen");

        let (_, agents) = get_json(app.clone(), "/api/agents").await;
        let model_of = |n: &str| {
            agents
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["name"] == n)
                .unwrap()["model"]
                .clone()
        };
        assert_eq!(model_of("claude"), "opus");
        assert_eq!(model_of("local"), "qwen");
        assert_eq!(model_of("agy"), "gemini-4-neon-medium");

        for bad in [
            json!({"claude": "a\nb"}),
            json!({"claude": "x".repeat(101)}),
            json!({"mock": "x"}),
        ] {
            let mut n = current.clone();
            n["agent_models"] = bad;
            let (status, _) = put_settings_json(app.clone(), n).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn api_task_model_priority_override_over_settings_over_default() {
        use std::sync::Arc;

        use forja_agent::{ClaudeAgent, OpencodeAgent};
        let (state, _s, _w) = state();
        // Programas inexistentes: ningún agente real se ejecuta.
        let state = state
            .with_agent(Arc::new(
                ClaudeAgent::default().with_program("/nonexistent/claude"),
            ))
            .with_agent(Arc::new(
                OpencodeAgent::default().with_program("/nonexistent/opencode"),
            ));
        let app = build_router(state);
        let (_, current) = get_json(app.clone(), "/api/settings").await;
        let mut next = current.clone();
        next["agent_models"] = json!({"claude": "opus"});
        assert_eq!(put_settings_json(app.clone(), next).await.0, StatusCode::OK);

        let task = |agent: &str, modelo: Option<&str>| {
            let mut t = json!({"titulo": "t", "prompt": "p", "agent": agent});
            if let Some(m) = modelo {
                t["modelo"] = json!(m);
            }
            t
        };
        // override por tarea > ajustes
        let (st, r) = post_task(app.clone(), task("claude", Some("haiku"))).await;
        assert_eq!(st, StatusCode::CREATED, "{r}");
        assert_eq!(r["model"], "haiku");
        // ajustes > default
        let (_, r) = post_task(app.clone(), task("claude", None)).await;
        assert_eq!(r["model"], "opus");
        // default en código
        let (_, r) = post_task(app.clone(), task("opencode", None)).await;
        assert_eq!(r["model"], "opencode-go/deepseek-v4.1-flash");
        // override inválido / agente sin modelo
        let (st, _) = post_task(app.clone(), task("claude", Some("--x"))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _) = post_task(app.clone(), task("mock", Some("x"))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        // mock sin modelo sigue funcionando y no registra modelo
        let (st, r) = post_task(app, task("mock", None)).await;
        assert_eq!(st, StatusCode::CREATED);
        assert!(r.get("model").is_none());
    }

    #[tokio::test]
    async fn api_effort_settings_agents_view_and_task_priority() {
        use std::sync::Arc;

        use forja_agent::{ClaudeAgent, HermesAgent, OpencodeAgent};
        let (state, _s, _w) = state();
        // Programas inexistentes: ningún agente real se ejecuta.
        let state = state
            .with_agent(Arc::new(
                ClaudeAgent::default().with_program("/nonexistent/claude"),
            ))
            .with_agent(Arc::new(
                OpencodeAgent::default().with_program("/nonexistent/opencode"),
            ))
            .with_agent(Arc::new(
                HermesAgent::default().with_program("/nonexistent/hermes"),
            ));
        let app = build_router(state);
        let (_, current) = get_json(app.clone(), "/api/settings").await;
        assert_eq!(current["agent_effort"], json!({}));

        let mut next = current.clone();
        next["agent_effort"] = json!({"claude": " LOW ", "hermes": "ultra", "opencode": "high"});
        next["agent_models"] = json!({"hermes": "gpt-x"});
        let (st, body) = put_settings_json(app.clone(), next.clone()).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["settings"]["agent_effort"]["claude"], "low");

        // /api/agents: esfuerzo efectivo, niveles por familia y model_editable.
        let (_, agents) = get_json(app.clone(), "/api/agents").await;
        let agent = |n: &str| {
            agents
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["name"] == n)
                .unwrap()
                .clone()
        };
        assert_eq!(agent("claude")["effort"], "low");
        assert_eq!(
            agent("claude")["effort_levels"],
            json!(["low", "medium", "high", "xhigh", "max"])
        );
        assert_eq!(agent("hermes")["effort"], "ultra");
        assert_eq!(
            agent("hermes")["effort_levels"].as_array().unwrap().len(),
            8
        );
        assert_eq!(agent("hermes")["model"], "gpt-x");
        assert!(agent("agy")["effort"].is_null());
        assert_eq!(agent("local")["effort_levels"], json!([]));
        assert_eq!(agent("mock")["effort_levels"], json!([]));
        assert_eq!(agent("local")["model_editable"], true);
        assert_eq!(agent("mock")["model_editable"], false);

        // Validación por familia en ajustes.
        for bad in [
            json!({"claude": "ultra"}),
            json!({"local": "high"}),
            json!({"hermes": "extreme"}),
            json!({"opencode": "a b"}),
        ] {
            let mut n = current.clone();
            n["agent_effort"] = bad;
            assert_eq!(
                put_settings_json(app.clone(), n).await.0,
                StatusCode::BAD_REQUEST
            );
        }

        // POST /tasks: tarea > ajustes > ninguno; queda en RunRecord.
        let task = |agent: &str, esfuerzo: Option<&str>| {
            let mut t = json!({"titulo": "t", "prompt": "p", "agent": agent});
            if let Some(e) = esfuerzo {
                t["esfuerzo"] = json!(e);
            }
            t
        };
        let (st, r) = post_task(app.clone(), task("claude", Some("max"))).await;
        assert_eq!(st, StatusCode::CREATED, "{r}");
        assert_eq!(r["effort"], "max");
        let (_, r) = post_task(app.clone(), task("claude", None)).await;
        assert_eq!(r["effort"], "low");
        let (_, r) = post_task(app.clone(), task("opencode", None)).await;
        assert_eq!(r["effort"], "high");
        let (_, r) = post_task(app.clone(), task("hermes", None)).await;
        assert_eq!(r["effort"], "ultra");
        assert_eq!(r["model"], "gpt-x");
        // Sin configurar no hay esfuerzo (y se omite del JSON).
        let (_, r) = post_task(app.clone(), task("agy", None)).await;
        assert!(r.get("effort").is_none(), "{r}");
        // Inválidos: valor de otra familia, local/mock, vacío ignorado.
        let (st, _) = post_task(app.clone(), task("claude", Some("ultra"))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, r) = post_task(app.clone(), task("local", Some("high"))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .contains("local no admite esfuerzo"),
            "{r}"
        );
        let (st, _) = post_task(app.clone(), task("mock", Some("high"))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        // Alias en español → valor canónico en el registro.
        let (st, r) = post_task(app.clone(), task("claude", Some("Muy alto"))).await;
        assert_eq!(st, StatusCode::CREATED, "{r}");
        assert_eq!(r["effort"], "xhigh");
        let (_, r) = post_task(app.clone(), task("opencode", Some("alto"))).await;
        assert_eq!(r["effort"], "high");
        let (st, r) = post_task(app.clone(), task("claude", Some("ninguno"))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(r["error"].as_str().unwrap().contains("low (bajo)"), "{r}");
        let (st, _) = post_task(app.clone(), task("claude", Some("  "))).await;
        assert_eq!(st, StatusCode::CREATED);
    }

    /// Estado con una base de Hermes falsa (claudia, yuuki, privado) y un orquestador
    /// con `claude` y `hermes` "instalados". Nunca toca el ~/.hermes real.
    fn state_with_fake_hermes() -> (AppState, TempDir, TempDir, TempDir) {
        use forja_agent::hermes::ProfileRules;

        use crate::orchestrator::{BackendFactory, Orchestrator};

        let home = TempDir::new().expect("hermes home");
        for p in ["claudia", "yuuki", "privado"] {
            std::fs::create_dir_all(home.path().join("profiles").join(p)).expect("perfil");
        }
        let rules = ProfileRules::new().with_base(home.path());
        let factory = BackendFactory::system(rules.clone())
            .with_probe(|a: &str| a == "claude" || a == "hermes");
        let (state, s, w) = state();
        let state = state
            .with_orchestrator(Orchestrator::from_factory(factory))
            .with_profile_rules(rules);
        (state, s, w, home)
    }

    #[tokio::test]
    async fn api_orchestrator_profile_hot_switch_discovery_and_hidden() {
        let (state, _s, _w, _home) = state_with_fake_hermes();
        let app = build_router(state);
        let (_, current) = get_json(app.clone(), "/api/settings").await;

        // Perfiles descubiertos y agentes disponibles; `allowed` == `profiles`.
        let (_, prof) = get_json(app.clone(), "/api/orchestrator/profile").await;
        assert_eq!(
            prof["profiles"],
            json!(["default", "claudia", "privado", "yuuki"])
        );
        assert_eq!(prof["allowed"], prof["profiles"]);
        assert_eq!(prof["agents_available"], json!(["claude", "hermes"]));
        assert!(prof["agent"].is_string() && prof["agent_source"].is_string());

        // Cambio en caliente a hermes con perfil (se normaliza a minúsculas).
        let mut next = current.clone();
        next["orchestrator_agent"] = json!("hermes");
        next["orchestrator_profile"] = json!("Claudia");
        let (st, body) = put_settings_json(app.clone(), next).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let (_, prof) = get_json(app.clone(), "/api/orchestrator/profile").await;
        assert_eq!(prof["profile"], "claudia");
        assert_eq!(prof["source"], "settings");
        assert_eq!(prof["agent"], "hermes");
        assert_eq!(prof["agent_source"], "settings");

        // Ocultar un perfil: desaparece de la lista y no se puede elegir.
        let mut hide = current.clone();
        hide["orchestrator_agent"] = json!("hermes");
        hide["orchestrator_profile"] = json!("claudia");
        hide["hidden_profiles"] = json!([" Privado "]);
        let (st, body) = put_settings_json(app.clone(), hide.clone()).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["settings"]["hidden_profiles"], json!(["privado"]));
        let (_, prof) = get_json(app.clone(), "/api/orchestrator/profile").await;
        assert_eq!(prof["profiles"], json!(["default", "claudia", "yuuki"]));

        // Oculto, inexistente o inválido: 400 y el perfil no cambia.
        for bad in ["privado", "otro", "../x"] {
            let mut n = hide.clone();
            n["orchestrator_profile"] = json!(bad);
            let (st, _) = put_settings_json(app.clone(), n).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
        }
        // Elegir y ocultar el mismo perfil a la vez también se rechaza.
        let mut n = current.clone();
        n["orchestrator_profile"] = json!("yuuki");
        n["hidden_profiles"] = json!(["yuuki"]);
        assert_eq!(
            put_settings_json(app.clone(), n).await.0,
            StatusCode::BAD_REQUEST
        );
        let (_, prof) = get_json(app.clone(), "/api/orchestrator/profile").await;
        assert_eq!(prof["profile"], "claudia");

        // Agente: otro válido se aplica; uno inválido da 400.
        let mut n = hide.clone();
        n["orchestrator_agent"] = json!("claude");
        assert_eq!(put_settings_json(app.clone(), n).await.0, StatusCode::OK);
        let (_, prof) = get_json(app.clone(), "/api/orchestrator/profile").await;
        assert_eq!(prof["agent"], "claude");
        let mut n = hide.clone();
        n["orchestrator_agent"] = json!("mock");
        assert_eq!(
            put_settings_json(app.clone(), n).await.0,
            StatusCode::BAD_REQUEST
        );

        // Quitar el ajuste de perfil vuelve a env/default.
        let mut n = hide.clone();
        n["orchestrator_agent"] = json!("hermes");
        n["orchestrator_profile"] = json!(null);
        let (st, body) = put_settings_json(app.clone(), n).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let (_, prof) = get_json(app, "/api/orchestrator/profile").await;
        let expected = crate::orchestrator::resolve_profile(
            None,
            std::env::var(crate::orchestrator::PROFILE_ENV)
                .ok()
                .as_deref(),
        );
        assert_eq!(prof["profile"], json!(expected));
        assert_ne!(prof["source"], "settings");
    }

    #[tokio::test]
    async fn api_models_hides_configured_hermes_profiles_even_when_cached() {
        use crate::models::{CommandRunner, ModelService};
        use std::sync::Arc;
        use std::time::Duration;

        struct HermesOnly;
        impl CommandRunner for HermesOnly {
            fn run(&self, program: &str, _args: &[&str]) -> Result<String, String> {
                if program == "hermes" {
                    Ok(" Profile  Model  Gateway\n  default  m1  running\n  privado  m2  running\n  claudia  m3  running\n".to_string())
                } else {
                    Err(format!("{program} no está instalado"))
                }
            }
        }
        let (state, _s, _w, _home) = state_with_fake_hermes();
        let state = state.with_models(Arc::new(ModelService::new(
            Arc::new(HermesOnly),
            Duration::from_secs(600),
        )));
        let app = build_router(state);
        let hermes_ids = |v: &Value| -> Vec<String> {
            v["providers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["provider"] == "hermes")
                .map(|p| {
                    p["models"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|m| m["id"].as_str().unwrap().to_string())
                        .collect()
                })
                .unwrap_or_default()
        };
        let (_, v) = get_json(app.clone(), "/api/models").await;
        assert!(hermes_ids(&v).contains(&"privado".to_string()));

        let (_, current) = get_json(app.clone(), "/api/settings").await;
        let mut n = current.clone();
        n["hidden_profiles"] = json!(["privado"]);
        assert_eq!(put_settings_json(app.clone(), n).await.0, StatusCode::OK);
        // La caché (600 s) no sirve la lista vieja.
        let (_, v) = get_json(app, "/api/models").await;
        assert_eq!(hermes_ids(&v), vec!["default", "claudia"]);
    }

    #[tokio::test]
    async fn api_telemetry_route_with_fake_sources() {
        use std::sync::Arc;

        use crate::telemetry::{GpuSample, GpuSource, SystemSource, Telemetry};

        struct FakeGpu;
        impl GpuSource for FakeGpu {
            fn read(&self) -> Result<GpuSample, String> {
                Ok(GpuSample {
                    name: "Fake RTX".to_string(),
                    memory_used_mib: 4096,
                    memory_total_mib: 16384,
                    utilization_percent: 42.0,
                    temperature_c: 55.0,
                })
            }
        }

        struct FakeSys;
        impl SystemSource for FakeSys {
            fn proc_stat(&self) -> Result<String, String> {
                Ok("cpu  100 0 50 850 0 0 0 0 0 0\n".to_string())
            }
            fn meminfo(&self) -> Result<String, String> {
                Ok("MemTotal:       32768000 kB\nMemAvailable:   16384000 kB\n".to_string())
            }
        }

        let (state, _s, _w) = state();
        let telemetry = Telemetry::new(Arc::new(FakeGpu), Arc::new(FakeSys), 10, 2000);
        let app = build_router(state.with_telemetry(telemetry));

        let (status, snap) = get_json(app, "/api/telemetry").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(snap["latest"]["gpu"]["name"], "Fake RTX");
        assert_eq!(snap["latest"]["gpu"]["memory_used_mib"], 4096);
        assert_eq!(snap["latest"]["ram_used_kib"], 16_384_000);
    }

    #[tokio::test]
    async fn api_quota_get_put_routes_with_fake_usage() {
        use std::sync::Arc;
        use std::time::Duration;

        use crate::quota::{QuotaService, UsageCommand};

        struct FakeUsage;
        impl UsageCommand for FakeUsage {
            fn usage_json(&self, provider: &str) -> Result<String, String> {
                if provider == "anthropic" {
                    Ok(include_str!("../tests/fixtures/hermes_usage_anthropic.json").to_string())
                } else {
                    Err("no configurado".to_string())
                }
            }
        }

        let (state, store_dir, _w) = state();
        let manual_path = store_dir.path().join("quota.json");
        let quota = Arc::new(QuotaService::new(
            Arc::new(FakeUsage),
            manual_path.clone(),
            Duration::from_secs(60),
        ));
        let app = build_router(state.with_quota(quota));

        let (status, initial) = get_json(app.clone(), "/api/quota").await;
        assert_eq!(status, StatusCode::OK);
        let providers = initial["providers"].as_array().expect("providers array");
        assert_eq!(providers.len(), 5);
        assert_eq!(providers[0]["provider"], "anthropic");
        assert_eq!(providers[0]["status"], "ok");
        assert_eq!(providers[0]["used_percent"], 65.0);
        assert_eq!(providers[1]["status"], "no_data");

        let payload = json!({
            "providers": {
                "deepseek": {
                    "balance": 78.24,
                    "currency": "USD",
                    "updated_at": "2026-11-26T05:00:00Z"
                },
                "gemini": {
                    "percent_remaining": 98.0,
                    "updated_at": "2026-11-26T05:00:00Z"
                }
            }
        });
        let req = Request::builder()
            .header(header::HOST, "127.0.0.1:7777")
            .method(Method::PUT)
            .uri("/api/quota")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string()))
            .expect("PUT /api/quota");
        let resp = app.clone().oneshot(req).await.expect("ejecutar PUT");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("cuerpo");
        let updated: Value = serde_json::from_slice(&bytes).expect("json");
        let upd_providers = updated["providers"].as_array().expect("providers");
        let deepseek = upd_providers
            .iter()
            .find(|p| p["provider"] == "deepseek")
            .expect("deepseek");
        assert_eq!(deepseek["status"], "ok");
        assert_eq!(deepseek["source"], "manual");
        assert_eq!(deepseek["balance"], 78.24);
        assert_eq!(deepseek["currency"], "USD");
        assert!(manual_path.exists());

        // Validación rechaza percent_remaining fuera de rango
        let bad = json!({ "providers": { "gemini": { "percent_remaining": 150.0 } } });
        let req_bad = Request::builder()
            .header(header::HOST, "127.0.0.1:7777")
            .method(Method::PUT)
            .uri("/api/quota")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bad.to_string()))
            .expect("PUT inválido");
        let resp_bad = app.oneshot(req_bad).await.expect("ejecutar PUT inválido");
        assert_eq!(resp_bad.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn api_sensitive_agents_persists_updates_policy_and_scheduler_live() {
        let (state, store_dir, _w) = state();
        let path = store_dir.path().join("settings.json");
        let app = build_router(state.with_settings_path(path.clone()));

        // Default: solo claude en /api/policy y /api/settings.
        let (status, policy) = get_json(app.clone(), "/api/policy").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(policy["sensitive"], json!(["claude"]));

        let (_, current) = get_json(app.clone(), "/api/settings").await;
        assert_eq!(current["sensitive_agents"], json!(["claude"]));

        // Con default, mock + sensitive se rechaza con 403.
        let (status, _) = post_task(
            app.clone(),
            json!({"titulo": "s1", "prompt": "p", "privacidad": "sensitive", "agent": "mock"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // PUT /api/settings con lista normalizada [" MOCK ", "Local"] persiste y aplica en caliente.
        let mut next = current.clone();
        next["sensitive_agents"] = json!([" MOCK ", "Local"]);
        let (status, body) = put_settings_json(app.clone(), next.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["persisted"], true);
        assert_eq!(body["restart_required"], json!([]));
        assert_eq!(
            body["settings"]["sensitive_agents"],
            json!(["mock", "local"])
        );

        // Persiste en settings.json.
        let loaded = crate::settings::load_settings(&path)
            .expect("leer settings")
            .expect("existe");
        assert_eq!(
            loaded.sensitive_agents,
            vec![AgentKind::Mock, AgentKind::Local]
        );

        // GET /api/policy refleja la lista efectiva.
        let (status, policy) = get_json(app.clone(), "/api/policy").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(policy["sensitive"], json!(["mock", "local"]));

        // Scheduler aplica en caliente sin reiniciar: mock ahora se acepta y agy/claude se deniegan.
        let (status, _) = post_task(
            app.clone(),
            json!({"titulo": "s2", "prompt": "p", "privacidad": "sensitive", "agent": "mock"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = post_task(
            app.clone(),
            json!({"titulo": "s3", "prompt": "p", "privacidad": "sensitive", "agent": "claude"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Inválidos (vacía, desconocido, duplicados) => 400 y no sobrescriben lo vigente.
        for bad_list in [json!([]), json!(["desconocido"]), json!(["mock", "MOCK"])] {
            let mut bad = current.clone();
            bad["sensitive_agents"] = bad_list.clone();
            let (status, _) = put_settings_json(app.clone(), bad).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "debía rechazar {bad_list}");
        }
        let (_, policy_after) = get_json(app, "/api/policy").await;
        assert_eq!(policy_after["sensitive"], json!(["mock", "local"]));
    }
}
