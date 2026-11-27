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
use forja_core::limits::{
    clip_middle, MAX_PROMPT_BYTES, MAX_STORED_ERROR_BYTES, MAX_STORED_OUTPUT_BYTES,
};
use forja_core::{redact_secrets, AgentKind, ForjaError, Result, RunRecord, RunStatus, Task};
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
    pub(crate) repo_path: std::sync::RwLock<Option<PathBuf>>,
    pub(crate) base_branch: std::sync::RwLock<String>,
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
    /// Identidad (`pid:arranque`) de este proceso: dueño de las ejecuciones que lanza.
    pub(crate) identity: String,
}

/// Error guardado en las ejecuciones que quedaron a medias porque el proceso de
/// Forja que las lanzó ya no existe (cierre abrupto, SIGKILL, apagón).
pub const INTERRUPTED_ERROR: &str =
    "interrumpida: el proceso de Forja que la ejecutaba terminó antes de que acabara";

/// Redacta secretos y recorta una salida antes de guardarla o mostrarla.
pub(crate) fn sanitize_text(text: &str, max_bytes: usize) -> String {
    clip_middle(&redact_secrets(text), max_bytes)
}

/// Deja un registro listo para memoria/disco: título y prompt sin secretos
/// (marcando `prompt_redacted` si cambió algo) y salida/error redactados y acotados.
pub(crate) fn sanitize_record(record: &mut RunRecord) {
    let titulo = redact_secrets(&record.task.titulo);
    let prompt = redact_secrets(&record.task.prompt);
    if titulo != record.task.titulo || prompt != record.task.prompt {
        record.prompt_redacted = true;
        record.task.titulo = titulo;
        record.task.prompt = prompt;
    }
    if let Some(out) = record.output.as_mut() {
        *out = sanitize_text(out, MAX_STORED_OUTPUT_BYTES);
    }
    if let Some(err) = record.error.as_mut() {
        *err = sanitize_text(err, MAX_STORED_ERROR_BYTES);
    }
}

fn is_active(status: RunStatus) -> bool {
    matches!(status, RunStatus::Pending | RunStatus::Running)
}

/// `true` si el run activo pertenece a otro proceso de Forja que sigue vivo
/// (p. ej. un `forja run` que comparte el store con el servidor).
fn owned_by_live_foreign(record: &RunRecord, me: &str) -> bool {
    record
        .owner
        .as_deref()
        .is_some_and(|owner| owner != me && forja_agent::process::identity_alive(owner))
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

/// Mientras viva no entran ejecuciones nuevas (ver [`Scheduler::idle_guard`]).
pub struct IdleGuard<'a> {
    _runs: tokio::sync::RwLockReadGuard<'a, HashMap<String, RunRecord>>,
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

impl Scheduler {
    pub fn new(store: JsonlStore, mut config: SchedulerConfig) -> Result<Self> {
        if config.max_concurrency == 0 {
            return Err(ForjaError::InvalidInput(
                "max_concurrency debe ser al menos 1".to_string(),
            ));
        }
        validate_sensitive_agents(&config.sensitive_agents)?;

        // Rutas absolutas: el cwd del proceso no debe cambiar dónde viven los worktrees.
        if config.worktrees_dir.is_relative() {
            config.worktrees_dir = std::path::absolute(&config.worktrees_dir)?;
        }

        let identity = forja_agent::process::current_process_identity();
        let existing_runs = store.list_runs()?;
        let mut runs_map = HashMap::with_capacity(existing_runs.len());
        for mut record in existing_runs {
            // Registros antiguos pudieron guardarse con secretos: no exponerlos.
            sanitize_record(&mut record);
            // Reconciliación: un run activo cuyo dueño ya no vive no avanzará nunca.
            if is_active(record.status) && !owned_by_live_foreign(&record, &identity) {
                record.status = RunStatus::Failed;
                record.error = Some(INTERRUPTED_ERROR.to_string());
                record.updated_at_ms = now_ms();
                store.append_run(&record)?;
            }
            runs_map.insert(record.id.clone(), record);
        }

        let limiter = ConcurrencyLimiter::new(config.max_concurrency);
        let repo_path = std::sync::RwLock::new(config.repo_path.clone());
        let base_branch = std::sync::RwLock::new(config.base_branch.clone());
        let sensitive_agents = std::sync::RwLock::new(config.sensitive_agents.clone());
        let (events, _) = broadcast::channel(128);

        Ok(Self {
            inner: Arc::new(SchedulerInner {
                config,
                repo_path,
                base_branch,
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
                identity,
            }),
        })
    }

    pub fn max_concurrency(&self) -> usize {
        self.inner.limiter.limit()
    }

    /// Repositorio git configurado para los worktrees (`None` sin `--repo`).
    pub fn repo_path(&self) -> Option<PathBuf> {
        self.inner
            .repo_path
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Rama base sobre la que se crean y fusionan las ramas de tarea.
    pub fn base_branch(&self) -> String {
        self.inner
            .base_branch
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Indica si existe alguna ejecución pendiente o en curso.
    pub async fn has_active_runs(&self) -> bool {
        self.idle_guard().await.is_none()
    }

    /// Si no hay ejecuciones activas (propias o de otro proceso vivo sobre el
    /// mismo store), devuelve un guard que impide dar de alta nuevas mientras
    /// exista. Sirve para cambios que no deben ocurrir con agentes en marcha.
    pub async fn idle_guard(&self) -> Option<IdleGuard<'_>> {
        self.refresh_foreign_runs().await;
        let external = {
            let _store_guard = self.inner.store_lock.lock().await;
            self.inner
                .store
                .list_runs()
                .unwrap_or_default()
                .iter()
                .any(|r| is_active(r.status) && owned_by_live_foreign(r, &self.inner.identity))
        };
        let runs = self.inner.runs.read().await;
        if external || runs.values().any(|r| is_active(r.status)) {
            return None;
        }
        Some(IdleGuard { _runs: runs })
    }

    /// Actualiza el repositorio git y la rama base activos si no hay ejecuciones activas.
    pub async fn set_workspace(
        &self,
        repo_path: Option<PathBuf>,
        base_branch: String,
    ) -> Result<()> {
        let Some(_idle) = self.idle_guard().await else {
            return Err(ForjaError::InvalidInput(
                "no se puede cambiar de carpeta mientras hay ejecuciones activas".to_string(),
            ));
        };
        *self
            .inner
            .repo_path
            .write()
            .unwrap_or_else(|e| e.into_inner()) = repo_path;
        *self
            .inner
            .base_branch
            .write()
            .unwrap_or_else(|e| e.into_inner()) = base_branch;
        Ok(())
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
        if task.prompt.len() > MAX_PROMPT_BYTES {
            return Err(ForjaError::InvalidInput(format!(
                "el prompt ocupa {} KiB y el máximo es {} KiB; resúmelo o pásale al agente \
                 un archivo del repo con el detalle",
                task.prompt.len().div_ceil(1024),
                MAX_PROMPT_BYTES / 1024
            )));
        }
        if let Some(reason) = agent.privacy_violation(task.privacidad) {
            return Err(ForjaError::InvalidInput(reason));
        }
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
        let mut record = RunRecord {
            id: run_id.clone(),
            task: task.clone(),
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
            repo: None,
            owner: Some(self.inner.identity.clone()),
            prompt_redacted: false,
        };
        // Lo que se guarda y se muestra va sin secretos; el agente recibe la tarea original.
        sanitize_record(&mut record);

        {
            let _store_guard = self.inner.store_lock.lock().await;
            let mut runs = self.inner.runs.write().await;
            // La política pudo cambiar mientras se esperaba el lock.
            forja_core::check_policy_with(task.privacidad, agent.kind(), &self.sensitive_agents())?;
            self.inner.store.append_run(&record)?;
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
            run_task(inner, task_run_id, task, agent).await;
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
        self.refresh_foreign_runs().await;
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
        self.refresh_foreign_runs().await;
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

    /// Apagado ordenado: aborta las ejecuciones de este proceso (al soltarse, el
    /// runner mata el grupo de procesos de cada agente) y las marca `cancelled`
    /// con `reason` en el error. No toca runs de otros procesos vivos.
    pub async fn shutdown(&self, reason: &str) {
        let pending: Vec<JoinHandle<()>> = {
            let mut handles = self.inner.handles.lock().await;
            handles.drain().map(|(_, handle)| handle).collect()
        };
        for handle in &pending {
            handle.abort();
        }
        for handle in pending {
            let _ = handle.await;
        }

        let _store_guard = self.inner.store_lock.lock().await;
        let mut runs = self.inner.runs.write().await;
        for record in runs.values_mut() {
            if !is_active(record.status) || owned_by_live_foreign(record, &self.inner.identity) {
                continue;
            }
            record.status = RunStatus::Cancelled;
            record.error = Some(reason.to_string());
            record.updated_at_ms = now_ms();
            let _ = self.inner.store.append_run(record);
            let _ = self.inner.events.send(record.clone());
            if let Some(out) = self.inner.outputs.get(&record.id) {
                out.finish();
            }
        }
    }

    /// Runs activos de otro proceso (visto al arrancar): si ese proceso terminó,
    /// toma el estado final del store; si lo dejó a medias, lo marca fallido.
    async fn refresh_foreign_runs(&self) {
        let me = &self.inner.identity;
        let stale: Vec<String> = {
            let runs = self.inner.runs.read().await;
            runs.values()
                .filter(|r| {
                    is_active(r.status)
                        && r.owner.as_deref().is_some_and(|o| o != me)
                        && !owned_by_live_foreign(r, me)
                })
                .map(|r| r.id.clone())
                .collect()
        };
        if stale.is_empty() {
            return;
        }
        let _store_guard = self.inner.store_lock.lock().await;
        let latest: HashMap<String, RunRecord> = self
            .inner
            .store
            .list_runs()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| stale.contains(&r.id))
            .map(|r| (r.id.clone(), r))
            .collect();
        let mut runs = self.inner.runs.write().await;
        for id in stale {
            let Some(record) = runs.get_mut(&id) else {
                continue;
            };
            if let Some(mut stored) = latest.get(&id).cloned() {
                sanitize_record(&mut stored);
                *record = stored;
            }
            if is_active(record.status) {
                record.status = RunStatus::Failed;
                record.error = Some(INTERRUPTED_ERROR.to_string());
                record.updated_at_ms = now_ms();
                let _ = self.inner.store.append_run(record);
            }
            let _ = self.inner.events.send(record.clone());
        }
    }

    /// Oculta a git las carpetas internas de Forja que vivan dentro de `repo`
    /// (worktrees y store) añadiéndolas a `.git/info/exclude`, sin tocar archivos
    /// versionados. Sirve para repos sin `.forja/` en su `.gitignore`.
    pub fn exclude_internal_dirs(&self, repo: &std::path::Path) -> Result<()> {
        worktree::exclude_internal_dirs(
            repo,
            &[
                self.inner.config.worktrees_dir.as_path(),
                self.inner.store.dir(),
            ],
        )
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
