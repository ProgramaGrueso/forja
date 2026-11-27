mod buffers;
mod concurrency;
mod config;
pub mod output;
mod runner;
mod worktree;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use forja_agent::Agent;
use forja_core::{AgentKind, ForjaError, Result, RunRecord, RunStatus, Task};
use forja_store::JsonlStore;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

use buffers::{push_lines, OutputBuffers};
use concurrency::ConcurrencyLimiter;
pub use config::SchedulerConfig;
pub use output::{is_valid_run_id, OutputChunk, OutputPage, OutputSubscription};
use output::{RunOutput, MAX_BYTES, MAX_CHUNKS};
use runner::run_task;

pub(crate) struct SchedulerInner {
    pub(crate) config: SchedulerConfig,
    pub(crate) store: JsonlStore,
    pub(crate) limiter: Arc<ConcurrencyLimiter>,
    pub(crate) sensitive_agents: std::sync::RwLock<Vec<AgentKind>>,
    pub(crate) runs: RwLock<HashMap<String, RunRecord>>,
    pub(crate) handles: Mutex<HashMap<String, JoinHandle<()>>>,
    pub(crate) store_lock: Mutex<()>,
    pub(crate) git_lock: Mutex<()>,
    pub(crate) events: broadcast::Sender<RunRecord>,
    pub(crate) outputs: OutputBuffers,
    out_max_chunks: AtomicUsize,
    out_max_bytes: AtomicUsize,
}

fn validate_sensitive_agents(agents: &[AgentKind]) -> Result<()> {
    if agents.is_empty() {
        return Err(ForjaError::InvalidInput(
            "sensitive_agents no puede estar vacío".to_string(),
        ));
    }
    let mut seen = Vec::with_capacity(agents.len());
    for &agent in agents {
        if seen.contains(&agent) {
            return Err(ForjaError::InvalidInput(format!(
                "sensitive_agents contiene agente duplicado: {agent}"
            )));
        }
        seen.push(agent);
    }
    Ok(())
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

impl Scheduler {
    pub fn new(store: JsonlStore, config: SchedulerConfig) -> Result<Self> {
        if config.max_concurrency == 0 {
            return Err(ForjaError::InvalidInput(
                "max_concurrency debe ser al menos 1".to_string(),
            ));
        }
        validate_sensitive_agents(&config.sensitive_agents)?;

        let existing_runs = store.list_runs()?;
        let mut runs_map = HashMap::with_capacity(existing_runs.len());
        for record in existing_runs {
            runs_map.insert(record.id.clone(), record);
        }

        let limiter = ConcurrencyLimiter::new(config.max_concurrency);
        let sensitive_agents = std::sync::RwLock::new(config.sensitive_agents.clone());
        let (events, _) = broadcast::channel(128);

        Ok(Self {
            inner: Arc::new(SchedulerInner {
                config,
                store,
                limiter,
                sensitive_agents,
                runs: RwLock::new(runs_map),
                handles: Mutex::new(HashMap::new()),
                store_lock: Mutex::new(()),
                git_lock: Mutex::new(()),
                events,
                outputs: OutputBuffers::default(),
                out_max_chunks: AtomicUsize::new(MAX_CHUNKS),
                out_max_bytes: AtomicUsize::new(MAX_BYTES),
            }),
        })
    }

    pub fn max_concurrency(&self) -> usize {
        self.inner.limiter.limit()
    }

    /// Repositorio git configurado para los worktrees (`None` sin `--repo`).
    pub fn repo_path(&self) -> Option<PathBuf> {
        self.inner.config.repo_path.clone()
    }

    /// Rama base sobre la que se crean y fusionan las ramas de tarea.
    pub fn base_branch(&self) -> String {
        self.inner.config.base_branch.clone()
    }

    /// Carpeta donde viven los worktrees de las ejecuciones.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.inner.config.worktrees_dir.clone()
    }

    /// Carpeta del store donde viven `runs.jsonl` y los datos persistidos.
    pub fn store_dir(&self) -> PathBuf {
        self.inner.store.dir().to_path_buf()
    }

    pub fn set_max_concurrency(&self, new_limit: usize) -> Result<()> {
        if new_limit == 0 {
            return Err(ForjaError::InvalidInput(
                "max_concurrency debe ser al menos 1".to_string(),
            ));
        }
        self.inner.limiter.set_limit(new_limit);
        Ok(())
    }

    /// Lista efectiva de agentes permitidos para privacidad `Sensitive`.
    pub fn sensitive_agents(&self) -> Vec<AgentKind> {
        self.inner
            .sensitive_agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Actualiza en caliente los agentes permitidos para privacidad `Sensitive`.
    pub fn set_sensitive_agents(&self, agents: Vec<AgentKind>) -> Result<()> {
        validate_sensitive_agents(&agents)?;
        *self
            .inner
            .sensitive_agents
            .write()
            .unwrap_or_else(|e| e.into_inner()) = agents;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RunRecord> {
        self.inner.events.subscribe()
    }

    /// Ajusta el tope del buffer de salida de los runs que se encolen a partir de ahora.
    pub fn set_output_limits(&self, max_chunks: usize, max_bytes: usize) {
        self.inner
            .out_max_chunks
            .store(max_chunks.max(1), Ordering::Relaxed);
        self.inner
            .out_max_bytes
            .store(max_bytes.max(1), Ordering::Relaxed);
    }

    /// Salida acumulada desde `since` (seq). `NotFound` si el id no es válido o no existe.
    pub async fn output_since(&self, run_id: &str, since: u64) -> Result<OutputPage> {
        let out = self.output_for(run_id).await?;
        Ok(out.page(since))
    }

    /// Suscripción en vivo a la salida de un run (empieza en `since`).
    pub async fn subscribe_output(&self, run_id: &str, since: u64) -> Result<OutputSubscription> {
        let out = self.output_for(run_id).await?;
        let rx = out.watch();
        Ok(OutputSubscription {
            out,
            rx,
            next_seq: since,
            done: false,
        })
    }

    async fn output_for(&self, run_id: &str) -> Result<Arc<RunOutput>> {
        if !is_valid_run_id(run_id) {
            return Err(ForjaError::NotFound(run_id.to_string()));
        }
        if let Some(out) = self.inner.outputs.get(run_id) {
            return Ok(out);
        }
        // Sin buffer en memoria (p. ej. tras reiniciar): reconstruir desde la salida guardada.
        let record = self
            .get_run(run_id)
            .await
            .ok_or_else(|| ForjaError::NotFound(run_id.to_string()))?;
        let out = RunOutput::new(MAX_CHUNKS, MAX_BYTES);
        if let Some(text) = record.output.as_deref() {
            push_lines(&out, "stdout", text);
        }
        if let Some(err) = record.error.as_deref() {
            push_lines(&out, "stderr", err);
        }
        out.finish();
        Ok(self.inner.outputs.register(run_id, out))
    }

    pub async fn submit(&self, task: Task, agent: Arc<dyn Agent>) -> Result<RunRecord> {
        {
            let allowed = self
                .inner
                .sensitive_agents
                .read()
                .unwrap_or_else(|e| e.into_inner());
            forja_core::check_policy_with(task.privacidad, agent.kind(), &allowed)?;
        }

        let now = now_ms();
        let run_id = Uuid::new_v4().to_string();
        let record = RunRecord {
            id: run_id.clone(),
            task,
            agent: agent.kind(),
            status: RunStatus::Pending,
            branch: None,
            worktree: None,
            attempts: 0,
            model: agent.model(),
            effort: agent.effort(),
            output: None,
            error: None,
            created_at_ms: now,
            updated_at_ms: now,
        };

        {
            let _store_guard = self.inner.store_lock.lock().await;
            self.inner.store.append_run(&record)?;
            let mut runs = self.inner.runs.write().await;
            runs.insert(run_id.clone(), record.clone());
            self.inner.outputs.register(
                &run_id,
                RunOutput::new(
                    self.inner.out_max_chunks.load(Ordering::Relaxed),
                    self.inner.out_max_bytes.load(Ordering::Relaxed),
                ),
            );
            let _ = self.inner.events.send(record.clone());
        }

        let inner = Arc::clone(&self.inner);
        let task_run_id = run_id.clone();

        let mut handles = self.inner.handles.lock().await;
        let handle = tokio::spawn(async move {
            run_task(inner, task_run_id, agent).await;
        });
        handles.insert(run_id, handle);

        Ok(record)
    }

    pub async fn wait_run(&self, run_id: &str) -> Result<RunRecord> {
        let handle = {
            let mut handles = self.inner.handles.lock().await;
            handles.remove(run_id)
        };

        if let Some(handle) = handle {
            let _ = handle.await;
        }

        let runs = self.inner.runs.read().await;
        runs.get(run_id)
            .cloned()
            .ok_or_else(|| ForjaError::NotFound(run_id.to_string()))
    }

    pub async fn wait_all(&self) {
        loop {
            let pending: Vec<JoinHandle<()>> = {
                let mut handles = self.inner.handles.lock().await;
                handles.drain().map(|(_, handle)| handle).collect()
            };

            if pending.is_empty() {
                break;
            }

            for handle in pending {
                let _ = handle.await;
            }
        }
    }

    pub async fn list_runs(&self) -> Vec<RunRecord> {
        let runs = self.inner.runs.read().await;
        let mut list: Vec<RunRecord> = runs.values().cloned().collect();
        list.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        list
    }

    pub async fn get_run(&self, run_id: &str) -> Option<RunRecord> {
        let runs = self.inner.runs.read().await;
        runs.get(run_id).cloned()
    }

    pub async fn cancel_run(&self, run_id: &str) -> Result<RunRecord> {
        {
            let mut handles = self.inner.handles.lock().await;
            if let Some(handle) = handles.remove(run_id) {
                handle.abort();
            }
        }

        let _store_guard = self.inner.store_lock.lock().await;
        let updated = {
            let mut runs = self.inner.runs.write().await;
            let Some(record) = runs.get_mut(run_id) else {
                return Err(ForjaError::NotFound(run_id.to_string()));
            };

            if matches!(record.status, RunStatus::Pending | RunStatus::Running) {
                record.status = RunStatus::Cancelled;
                record.updated_at_ms = now_ms();
                Some(record.clone())
            } else {
                return Ok(record.clone());
            }
        };

        if let Some(record) = updated {
            self.inner.store.append_run(&record)?;
            let _ = self.inner.events.send(record.clone());
            if let Some(out) = self.inner.outputs.get(run_id) {
                out.finish();
            }
            Ok(record)
        } else {
            Err(ForjaError::NotFound(run_id.to_string()))
        }
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
