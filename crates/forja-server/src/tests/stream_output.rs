use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use forja_agent::{Agent, MockAgent};
use forja_core::{AgentKind, Privacy, RunStatus};
use serde_json::Value;
use tokio_stream::StreamExt;
use tower::ServiceExt;

use super::{get, read_json, test_state};
use crate::build_router;

struct ScriptAgent(Vec<(String, String)>);

impl Agent for ScriptAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Mock
    }

    fn run<'a>(&'a self, ctx: &'a forja_agent::RunContext) -> forja_agent::AgentFuture<'a> {
        Box::pin(async move {
            let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../forja-agent/tests/fake_stream.sh");
            let argv = vec![script.to_string_lossy().into_owned()];
            let env: Vec<(&str, &str)> = self
                .0
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let res = forja_agent::Runner::run_argv_with_env(
                &argv,
                &ctx.worktree,
                ctx.timeout_secs,
                &env,
            )
            .await?;
            Ok(forja_agent::AgentOutput {
                text: res.stdout.trim().to_string(),
                raw_stdout: res.stdout,
                raw_stderr: res.stderr,
                exit_code: res.exit_code,
                warnings: Vec::new(),
            })
        })
    }
}

fn script_agent(lines: &str, sleep: &str) -> Arc<dyn Agent> {
    Arc::new(ScriptAgent(vec![
        ("FAKE_STREAM_LINES".into(), lines.into()),
        ("FAKE_STREAM_SLEEP".into(), sleep.into()),
    ]))
}

#[tokio::test]
async fn events_sse_streams_run_updates() {
    let (state, _store_dir, _wt_dir) = test_state();
    let scheduler = state.scheduler.clone();
    let app = build_router(state);

    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
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

#[tokio::test]
async fn run_output_endpoint_supports_since() {
    let (state, _s, _w) = test_state();
    let scheduler = state.scheduler.clone();
    let app = build_router(state);
    let run = scheduler
        .submit(
            forja_core::Task::new("out", "p", Privacy::Open),
            script_agent("4", "0"),
        )
        .await
        .expect("submit");
    scheduler.wait_run(&run.id).await.expect("wait");

    let res = get(&app, &format!("/runs/{}/output", run.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v: Value = read_json(res.into_body()).await;
    assert_eq!(v["chunks"].as_array().unwrap().len(), 4);
    assert_eq!(v["next_seq"], 4);
    assert_eq!(v["finished"], true);
    assert_eq!(v["chunks"][0]["stream"], "stdout");
    assert_eq!(v["chunks"][0]["text"], "linea 1\n");

    let res = get(&app, &format!("/runs/{}/output?since=3", run.id)).await;
    let v: Value = read_json(res.into_body()).await;
    assert_eq!(v["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(v["chunks"][0]["seq"], 3);
    assert_eq!(v["next_seq"], 4);
}

#[tokio::test]
async fn run_output_and_stream_unknown_id_404() {
    let (state, _s, _w) = test_state();
    let app = build_router(state);
    for uri in [
        "/runs/no-existe/output",
        "/runs/no-existe/stream",
        "/runs/..%2Fetc/output",
        "/runs/..%2Fetc/stream",
    ] {
        let res = get(&app, uri).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn run_stream_sse_live_then_status_and_close() {
    let (state, _s, _w) = test_state();
    let scheduler = state.scheduler.clone();
    let app = build_router(state);
    let run = scheduler
        .submit(
            forja_core::Task::new("sse", "p", Privacy::Open),
            script_agent("3", "0.15"),
        )
        .await
        .expect("submit");

    let res = get(&app, &format!("/runs/{}/stream", run.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/event-stream"));

    let mut body = res.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("sin timeout")
        .expect("chunk")
        .expect("ok");
    let first = String::from_utf8(first.to_vec()).unwrap();
    assert!(first.contains("event: output"), "{first}");
    assert!(first.contains("linea 1"), "{first}");
    assert_eq!(
        scheduler.get_run(&run.id).await.unwrap().status,
        RunStatus::Running,
        "el primer evento llega con el run aún en curso"
    );

    let mut rest = first;
    while let Some(Ok(b)) = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("sin timeout")
    {
        rest.push_str(&String::from_utf8_lossy(&b));
    }
    assert!(rest.contains("linea 3"));
    assert!(rest.contains("event: status"));
    assert!(
        rest.contains("\"status\":\"done\"") || rest.contains("\"status\":\"Done\""),
        "{rest}"
    );
}

#[tokio::test]
async fn run_stream_finished_run_replays_and_closes() {
    let (state, _s, _w) = test_state();
    let scheduler = state.scheduler.clone();
    let app = build_router(state);
    let run = scheduler
        .submit(
            forja_core::Task::new("sse2", "p", Privacy::Open),
            script_agent("2", "0"),
        )
        .await
        .expect("submit");
    scheduler.wait_run(&run.id).await.expect("wait");

    let res = get(&app, &format!("/runs/{}/stream?since=1", run.id)).await;
    let mut body = res.into_body().into_data_stream();
    let mut all = String::new();
    while let Some(Ok(b)) = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("el stream debe cerrar")
    {
        all.push_str(&String::from_utf8_lossy(&b));
    }
    let outputs: Vec<&str> = all.matches("event: output").collect();
    assert_eq!(outputs.len(), 1, "since=1 salta el primero: {all}");
    assert!(all.contains("linea 2"));
    assert!(all.contains("event: status"));
}
