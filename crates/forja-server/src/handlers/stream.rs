use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::handlers::ApiError;
use crate::state::AppState;

pub(crate) async fn events_handler(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.scheduler.subscribe()).filter_map(|msg| match msg {
        Ok(record) => Event::default().event("run").json_data(record).ok().map(Ok),
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

#[derive(Debug, Deserialize)]
pub(crate) struct OutputQuery {
    #[serde(default)]
    since: u64,
}

#[derive(Debug, Serialize)]
struct OutputChunkDto {
    seq: u64,
    stream: String,
    text: String,
}

impl From<forja_sched::OutputChunk> for OutputChunkDto {
    fn from(c: forja_sched::OutputChunk) -> Self {
        Self {
            seq: c.seq,
            stream: c.stream,
            text: c.text,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct OutputResponse {
    chunks: Vec<OutputChunkDto>,
    next_seq: u64,
    truncated: bool,
    finished: bool,
}

/// GET /runs/{id}/output?since=N: salida acumulada (ya redactada) para reconectar.
pub(crate) async fn run_output(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<OutputQuery>,
) -> std::result::Result<Json<OutputResponse>, ApiError> {
    let page = state.scheduler.output_since(&id, q.since).await?;
    Ok(Json(OutputResponse {
        chunks: page.chunks.into_iter().map(Into::into).collect(),
        next_seq: page.next_seq,
        truncated: page.truncated,
        finished: page.finished,
    }))
}

/// GET /runs/{id}/stream?since=N: SSE con eventos `output` en vivo y un `status`
/// final con el RunRecord; si el run ya terminó reproduce el buffer y cierra.
pub(crate) async fn run_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<OutputQuery>,
) -> std::result::Result<
    Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>>,
    ApiError,
> {
    let mut sub = state.scheduler.subscribe_output(&id, q.since).await?;
    let scheduler = state.scheduler.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);

    tokio::spawn(async move {
        while let Some(batch) = sub.next_batch().await {
            for chunk in batch {
                let seq = chunk.seq;
                let Ok(ev) = Event::default()
                    .event("output")
                    .id(seq.to_string())
                    .json_data(OutputChunkDto::from(chunk))
                else {
                    continue;
                };
                if tx.send(ev).await.is_err() {
                    return;
                }
            }
        }
        if let Some(record) = scheduler.get_run(&id).await {
            if let Ok(ev) = Event::default().event("status").json_data(record) {
                let _ = tx.send(ev).await;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
