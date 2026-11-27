use std::path::Path;
use std::sync::Arc;

use forja_agent::{with_output_sink, Agent, OutputSink, OutputStream, RunContext};
use forja_core::limits::{MAX_STORED_ERROR_BYTES, MAX_STORED_OUTPUT_BYTES};
use forja_core::{Result, RunStatus, Task};

use crate::buffers::push_lines;
use crate::worktree::{commit_worktree_changes, prepare_worktree};
use crate::{now_ms, sanitize_text, SchedulerInner};

/// Ejecuta `task` (la original, con el prompt completo: el registro guardado
/// puede llevarlo redactado) y va actualizando el registro `run_id`.
pub(crate) async fn run_task(
    inner: Arc<SchedulerInner>,
    run_id: String,
    task: Task,
    agent: Arc<dyn Agent>,
) {
    let _permit = inner.limiter.acquire().await;

    let wt_path = inner.config.worktrees_dir.join(&run_id);
    let repo_path = inner
        .repo_path
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let base_branch = inner
        .base_branch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let branch = repo_path.as_ref().map(|_| format!("forja/{run_id}"));

    {
        let _store_guard = inner.store_lock.lock().await;
        let maybe_record = {
            let mut runs = inner.runs.write().await;
            let Some(record) = runs.get_mut(&run_id) else {
                return;
            };
            if record.status == RunStatus::Cancelled {
                return;
            }
            record.status = RunStatus::Running;
            record.attempts = 1;
            record.branch = branch.clone();
            record.worktree = Some(wt_path.to_string_lossy().into_owned());
            record.repo = repo_path
                .as_ref()
                .map(|repo| repo.canonicalize().unwrap_or_else(|_| repo.clone()))
                .map(|repo| repo.to_string_lossy().into_owned());
            record.updated_at_ms = now_ms();
            record.clone()
        };
        let _ = inner.store.append_run(&maybe_record);
        let _ = inner.events.send(maybe_record);
    }

    let execution_result = execute_with_retries(
        &inner,
        &run_id,
        &task,
        &agent,
        &wt_path,
        repo_path.as_deref(),
        &base_branch,
        branch.as_deref(),
    )
    .await;

    {
        let _store_guard = inner.store_lock.lock().await;
        let updated = {
            let mut runs = inner.runs.write().await;
            let Some(record) = runs.get_mut(&run_id) else {
                return;
            };
            if record.status == RunStatus::Cancelled {
                return;
            }
            match execution_result {
                Ok(output) => {
                    record.status = RunStatus::Done;
                    record.output = Some(sanitize_text(&output, MAX_STORED_OUTPUT_BYTES));
                    record.error = None;
                }
                Err(err) => {
                    record.status = RunStatus::Failed;
                    record.error = Some(sanitize_text(&err.to_string(), MAX_STORED_ERROR_BYTES));
                }
            }
            record.updated_at_ms = now_ms();
            record.clone()
        };
        let _ = inner.store.append_run(&updated);
        let _ = inner.events.send(updated);
    }

    if let Some(out) = inner.outputs.get(&run_id) {
        out.finish();
    }

    let mut handles = inner.handles.lock().await;
    handles.remove(&run_id);
}

#[allow(clippy::too_many_arguments)]
async fn execute_with_retries(
    inner: &SchedulerInner,
    run_id: &str,
    task: &Task,
    agent: &Arc<dyn Agent>,
    wt_path: &Path,
    repo_path: Option<&Path>,
    base_branch: &str,
    branch: Option<&str>,
) -> Result<String> {
    prepare_worktree(
        repo_path,
        &inner.config.worktrees_dir,
        inner.store.dir(),
        base_branch,
        &inner.git_lock,
        wt_path,
        branch,
    )
    .await?;

    let ctx = RunContext {
        task: task.clone(),
        worktree: wt_path.to_path_buf(),
        timeout_secs: inner.config.timeout_secs,
    };

    let out = inner.outputs.get(run_id);
    let sink: Option<OutputSink> = out.as_ref().map(|out| {
        let out = Arc::clone(out);
        Arc::new(move |stream: OutputStream, text: String| out.push(stream.as_str(), &text))
            as OutputSink
    });

    let mut attempt: u32 = 1;
    let agent_output = loop {
        let seen_before = out.as_ref().map_or(0, |o| o.page(0).next_seq);
        let result = match &sink {
            Some(sink) => with_output_sink(Arc::clone(sink), agent.run(&ctx)).await,
            None => agent.run(&ctx).await,
        };
        // Agentes sin streaming: emitir la salida completa al terminar el intento.
        if let Some(out) = &out {
            if out.page(0).next_seq == seen_before {
                match &result {
                    Ok(o) => {
                        let stdout = if o.raw_stdout.is_empty() {
                            &o.text
                        } else {
                            &o.raw_stdout
                        };
                        push_lines(out, "stdout", stdout);
                        push_lines(out, "stderr", &o.raw_stderr);
                    }
                    Err(e) => push_lines(out, "stderr", &format!("{e}\n")),
                }
            }
        }
        match result {
            Ok(output) => {
                if output
                    .warnings
                    .iter()
                    .any(|w| w == forja_agent::EFFORT_NOT_APPLIED_WARNING)
                {
                    // El esfuerzo no se aplicó: el registro no debe mentir.
                    let _store_guard = inner.store_lock.lock().await;
                    let updated = {
                        let mut runs = inner.runs.write().await;
                        runs.get_mut(run_id).map(|r| {
                            r.effort = None;
                            r.updated_at_ms = now_ms();
                            r.clone()
                        })
                    };
                    if let Some(rec) = updated {
                        let _ = inner.store.append_run(&rec);
                        let _ = inner.events.send(rec);
                    }
                }
                break output;
            }
            Err(err) => {
                if attempt <= inner.config.max_retries {
                    tokio::time::sleep(inner.config.retry_backoff).await;
                    attempt = attempt.saturating_add(1);

                    let _store_guard = inner.store_lock.lock().await;
                    let maybe_record = {
                        let mut runs = inner.runs.write().await;
                        let Some(record) = runs.get_mut(run_id) else {
                            return Err(err);
                        };
                        if record.status == RunStatus::Cancelled {
                            return Err(err);
                        }
                        record.attempts = attempt;
                        record.error =
                            Some(sanitize_text(&err.to_string(), MAX_STORED_ERROR_BYTES));
                        record.updated_at_ms = now_ms();
                        record.clone()
                    };
                    let _ = inner.store.append_run(&maybe_record);
                    let _ = inner.events.send(maybe_record);
                } else {
                    return Err(err);
                }
            }
        }
    };

    if repo_path.is_some() {
        commit_worktree_changes(wt_path, &task.titulo).await?;
    }

    Ok(agent_output.text)
}
