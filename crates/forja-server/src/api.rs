//! Rutas `/api/*` del dashboard: agentes, estadísticas, política, salud y ajustes.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use forja_core::{allowed_agents, AgentKind, Privacy, RunRecord, RunStatus};
use serde::Serialize;
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
    pub available: bool,
}

impl AgentMeta {
    pub fn default_for(kind: AgentKind, available: bool) -> Self {
        let (provider, model) = match kind {
            AgentKind::Mock => ("Local", "mock"),
            AgentKind::Agy => ("Gemini", forja_agent::agy::DEFAULT_MODEL),
            AgentKind::Claude => ("Anthropic", "sin configurar"),
        };
        Self {
            kind,
            provider: provider.to_string(),
            model: model.to_string(),
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

pub async fn agents(State(state): State<AppState>) -> Json<Vec<AgentView>> {
    let runs = state.scheduler.list_runs().await;
    Json(build_agent_views(&state.metas, &runs))
}

pub async fn stats(State(state): State<AppState>) -> Json<Stats> {
    let runs = state.scheduler.list_runs().await;
    Json(build_stats(&runs, now_ms()))
}

pub async fn policy() -> Json<serde_json::Value> {
    Json(json!({
        "open": allowed_agents(Privacy::Open),
        "sensitive": allowed_agents(Privacy::Sensitive),
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
    let Json(next) = match body {
        Ok(json) => json,
        Err(err) => return error_response(StatusCode::BAD_REQUEST, err.body_text()),
    };
    if let Err(msg) = next.validate() {
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

        let (status, health) = get_json(app, "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(health["version"], VERSION);
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
            .method(Method::PUT)
            .uri("/api/settings")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{roto"))
            .expect("request roto");
        let resp = app.oneshot(req).await.expect("PUT roto");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
            .method(Method::PUT)
            .uri("/api/quota")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bad.to_string()))
            .expect("PUT inválido");
        let resp_bad = app.oneshot(req_bad).await.expect("ejecutar PUT inválido");
        assert_eq!(resp_bad.status(), StatusCode::BAD_REQUEST);
    }
}
