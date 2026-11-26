pub mod output;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use forja_agent::{with_output_sink, Agent, OutputSink, OutputStream, RunContext};
use forja_core::{ForjaError, Result, RunRecord, RunStatus, Task};
use forja_store::JsonlStore;
use tokio::sync::{broadcast, Mutex, Notify, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub use output::{is_valid_run_id, OutputChunk, OutputPage, OutputSubscription};
use output::{RunOutput, MAX_BYTES, MAX_CHUNKS};

/// Máximo de buffers de salida de runs terminados que se conservan en memoria.
const MAX_RETAINED_OUTPUTS: usize = 256;

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub max_concurrency: usize,
    pub timeout_secs: u64,
    pub worktrees_dir: PathBuf,
    pub repo_path: Option<PathBuf>,
    pub base_branch: String,
    pub max_retries: u32,
    pub retry_backoff: Duration,
}

impl SchedulerConfig {
    pub fn new(worktrees_dir: impl Into<PathBuf>) -> Self {
        Self {
            max_concurrency: 2,
            timeout_secs: 300,
            worktrees_dir: worktrees_dir.into(),
            repo_path: None,
            base_branch: "main".to_string(),
            max_retries: 2,
            retry_backoff: Duration::from_millis(20),
        }
    }
}

struct ConcurrencyState {
    active: usize,
    limit: usize,
}

struct ConcurrencyLimiter {
    state: std::sync::Mutex<ConcurrencyState>,
    notify: Notify,
}

struct ConcurrencyPermit {
    limiter: Arc<ConcurrencyLimiter>,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        let mut st = self.limiter.state.lock().unwrap_or_else(|e| e.into_inner());
        st.active = st.active.saturating_sub(1);
        drop(st);
        self.limiter.notify.notify_waiters();
    }
}

impl ConcurrencyLimiter {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(ConcurrencyState { active: 0, limit }),
            notify: Notify::new(),
        })
    }

    async fn acquire(self: &Arc<Self>) -> ConcurrencyPermit {
        loop {
            let notified = self.notify.notified();
            {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if st.active < st.limit {
                    st.active += 1;
                    return ConcurrencyPermit {
                        limiter: Arc::clone(self),
                    };
                }
            }
            notified.await;
        }
    }

    fn set_limit(&self, new_limit: usize) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.limit = new_limit;
        drop(st);
        self.notify.notify_waiters();
    }

    fn limit(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).limit
    }
}

struct SchedulerInner {
    config: SchedulerConfig,
    store: JsonlStore,
    limiter: Arc<ConcurrencyLimiter>,
    runs: RwLock<HashMap<String, RunRecord>>,
    handles: Mutex<HashMap<String, JoinHandle<()>>>,
    store_lock: Mutex<()>,
    git_lock: Mutex<()>,
    events: broadcast::Sender<RunRecord>,
    outputs: std::sync::Mutex<OutputBuffers>,
    out_max_chunks: AtomicUsize,
    out_max_bytes: AtomicUsize,
}

#[derive(Default)]
struct OutputBuffers {
    map: HashMap<String, Arc<RunOutput>>,
    order: VecDeque<String>,
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

        let existing_runs = store.list_runs()?;
        let mut runs_map = HashMap::with_capacity(existing_runs.len());
        for record in existing_runs {
            runs_map.insert(record.id.clone(), record);
        }

        let limiter = ConcurrencyLimiter::new(config.max_concurrency);
        let (events, _) = broadcast::channel(128);

        Ok(Self {
            inner: Arc::new(SchedulerInner {
                config,
                store,
                limiter,
                runs: RwLock::new(runs_map),
                handles: Mutex::new(HashMap::new()),
                store_lock: Mutex::new(()),
                git_lock: Mutex::new(()),
                events,
                outputs: std::sync::Mutex::new(OutputBuffers::default()),
                out_max_chunks: AtomicUsize::new(MAX_CHUNKS),
                out_max_bytes: AtomicUsize::new(MAX_BYTES),
            }),
        })
    }

    pub fn max_concurrency(&self) -> usize {
        self.inner.limiter.limit()
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
        if let Some(out) = self.inner.output_buffer(run_id) {
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
        Ok(self.inner.register_output(run_id, out))
    }

    pub async fn submit(&self, task: Task, agent: Arc<dyn Agent>) -> Result<RunRecord> {
        forja_core::check_policy(task.privacidad, agent.kind())?;

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
            self.inner.register_output(
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
            if let Some(out) = self.inner.output_buffer(run_id) {
                out.finish();
            }
            Ok(record)
        } else {
            Err(ForjaError::NotFound(run_id.to_string()))
        }
    }
}

impl SchedulerInner {
    fn output_buffer(&self, run_id: &str) -> Option<Arc<RunOutput>> {
        let bufs = self.outputs.lock().unwrap_or_else(|e| e.into_inner());
        bufs.map.get(run_id).cloned()
    }

    /// Registra un buffer y descarta los más antiguos ya terminados si hay demasiados.
    fn register_output(&self, run_id: &str, out: Arc<RunOutput>) -> Arc<RunOutput> {
        let mut bufs = self.outputs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = bufs.map.get(run_id) {
            return Arc::clone(existing);
        }
        bufs.map.insert(run_id.to_string(), Arc::clone(&out));
        bufs.order.push_back(run_id.to_string());
        let mut idx = 0;
        while bufs.map.len() > MAX_RETAINED_OUTPUTS && idx < bufs.order.len() {
            let id = bufs.order[idx].clone();
            if bufs.map.get(&id).is_some_and(|o| o.is_finished()) && id != run_id {
                bufs.map.remove(&id);
                bufs.order.remove(idx);
            } else {
                idx += 1;
            }
        }
        out
    }
}

fn push_lines(out: &RunOutput, stream: &str, text: &str) {
    for line in text.split_inclusive('\n') {
        out.push(stream, line);
    }
}

async fn run_task(inner: Arc<SchedulerInner>, run_id: String, agent: Arc<dyn Agent>) {
    let _permit = inner.limiter.acquire().await;

    let wt_path = inner.config.worktrees_dir.join(&run_id);
    let branch = inner
        .config
        .repo_path
        .as_ref()
        .map(|_| format!("forja/{run_id}"));

    let task = {
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
            record.updated_at_ms = now_ms();
            record.clone()
        };
        let _ = inner.store.append_run(&maybe_record);
        let _ = inner.events.send(maybe_record.clone());
        maybe_record.task
    };

    let execution_result =
        execute_with_retries(&inner, &run_id, &task, &agent, &wt_path, branch.as_deref()).await;

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
                    record.output = Some(output);
                    record.error = None;
                }
                Err(err) => {
                    record.status = RunStatus::Failed;
                    record.error = Some(err.to_string());
                }
            }
            record.updated_at_ms = now_ms();
            record.clone()
        };
        let _ = inner.store.append_run(&updated);
        let _ = inner.events.send(updated);
    }

    if let Some(out) = inner.output_buffer(&run_id) {
        out.finish();
    }

    let mut handles = inner.handles.lock().await;
    handles.remove(&run_id);
}

async fn prepare_worktree(
    inner: &SchedulerInner,
    wt_path: &Path,
    branch: Option<&str>,
) -> Result<()> {
    if let (Some(repo), Some(branch_name)) = (inner.config.repo_path.as_ref(), branch) {
        tokio::fs::create_dir_all(&inner.config.worktrees_dir).await?;

        let repo_owned = repo.clone();
        let base_branch = inner.config.base_branch.clone();
        let branch_owned = branch_name.to_string();
        let wt_owned = wt_path.to_path_buf();

        let _git_guard = inner.git_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            forja_git::create_worktree(&repo_owned, &base_branch, &branch_owned, &wt_owned)
        })
        .await
        .map_err(|err| ForjaError::Git(format!("fallo al crear worktree: {err}")))??;
    } else {
        tokio::fs::create_dir_all(wt_path).await?;
    }

    Ok(())
}

async fn execute_with_retries(
    inner: &SchedulerInner,
    run_id: &str,
    task: &Task,
    agent: &Arc<dyn Agent>,
    wt_path: &Path,
    branch: Option<&str>,
) -> Result<String> {
    prepare_worktree(inner, wt_path, branch).await?;

    let ctx = RunContext {
        task: task.clone(),
        worktree: wt_path.to_path_buf(),
        timeout_secs: inner.config.timeout_secs,
    };

    let out = inner.output_buffer(run_id);
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
            Ok(output) => break output,
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
                        record.error = Some(err.to_string());
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

    if inner.config.repo_path.is_some() {
        let wt_owned = wt_path.to_path_buf();
        let commit_msg = format!("forja: {}", task.titulo);
        tokio::task::spawn_blocking(move || -> Result<()> {
            if !forja_git::status_clean(&wt_owned)? {
                let _ = forja_git::commit_all(&wt_owned, &commit_msg)?;
            }
            Ok(())
        })
        .await
        .map_err(|err| ForjaError::Git(format!("fallo al commitear cambios: {err}")))??;
    }

    Ok(agent_output.text)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use forja_agent::{Agent, AgentFuture, AgentOutput, AgyAgent, MockAgent, RunContext};
    use forja_core::{AgentKind, ForjaError, Privacy, RunStatus, Task};
    use forja_store::JsonlStore;
    use tempfile::TempDir;

    use super::{Scheduler, SchedulerConfig};

    struct CountingAgent {
        active: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl CountingAgent {
        fn new(active: Arc<AtomicUsize>, max_observed: Arc<AtomicUsize>, delay: Duration) -> Self {
            Self {
                active,
                max_observed,
                delay,
            }
        }
    }

    impl Agent for CountingAgent {
        fn kind(&self) -> AgentKind {
            AgentKind::Mock
        }

        fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
            let active = Arc::clone(&self.active);
            let max_observed = Arc::clone(&self.max_observed);
            let delay = self.delay;
            let titulo = ctx.task.titulo.clone();

            Box::pin(async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_observed.fetch_max(current, Ordering::SeqCst);

                tokio::time::sleep(delay).await;

                active.fetch_sub(1, Ordering::SeqCst);

                Ok(AgentOutput {
                    text: format!("ok: {titulo}"),
                    raw_stdout: format!("ok: {titulo}"),
                    raw_stderr: String::new(),
                    exit_code: Some(0),
                    warnings: Vec::new(),
                })
            })
        }
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("ejecutar git en test");
        assert!(
            output.status.success(),
            "git {:?} falló:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_temp_repo() -> TempDir {
        let dir = TempDir::new().expect("crear directorio temporal");
        let repo = dir.path();

        run_git(repo, &["init", "-b", "main"]);
        run_git(repo, &["config", "user.name", "Forja Test"]);
        run_git(repo, &["config", "user.email", "forja-test@example.com"]);

        fs::write(repo.join("README.md"), "# Repo de prueba\n").expect("escribir README");
        run_git(repo, &["add", "README.md"]);
        run_git(repo, &["commit", "-m", "commit inicial"]);

        dir
    }

    #[tokio::test]
    async fn concurrency_limits_parallel_runs_to_n() {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

        let mut config = SchedulerConfig::new(wt_dir.path());
        config.max_concurrency = 2;

        let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");

        let active = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));
        let agent: Arc<dyn Agent> = Arc::new(CountingAgent::new(
            Arc::clone(&active),
            Arc::clone(&max_observed),
            Duration::from_millis(60),
        ));

        let mut submitted_ids = Vec::new();
        for i in 0..6 {
            let task = Task::new(format!("Tarea {i}"), format!("Prompt {i}"), Privacy::Open);
            let record = scheduler
                .submit(task, Arc::clone(&agent))
                .await
                .expect("submit debe tener éxito");
            assert_eq!(record.status, RunStatus::Pending);
            submitted_ids.push(record.id);
        }

        scheduler.wait_all().await;

        let observed = max_observed.load(Ordering::SeqCst);
        assert!(
            observed <= 2,
            "el máximo de ejecuciones simultáneas fue {observed}, superando max_concurrency = 2"
        );
        assert!(
            observed >= 1,
            "al menos una ejecución debió ejecutarse activamente"
        );
        assert_eq!(active.load(Ordering::SeqCst), 0);

        let mem_runs = scheduler.list_runs().await;
        assert_eq!(mem_runs.len(), 6);
        for id in &submitted_ids {
            let run = scheduler.get_run(id).await.expect("run en memoria");
            assert_eq!(run.status, RunStatus::Done);
            assert!(run.output.is_some());
            assert_eq!(run.attempts, 1);
        }

        let stored_runs = store.list_runs().expect("listar runs del store");
        assert_eq!(stored_runs.len(), 6);
        for run in stored_runs {
            assert_eq!(run.status, RunStatus::Done);
        }
    }

    #[tokio::test]
    async fn concurrency_cancel_run_marks_cancelled() {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

        let config = SchedulerConfig::new(wt_dir.path());
        let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");

        let slow_agent: Arc<dyn Agent> =
            Arc::new(MockAgent::success("lento").with_delay(Duration::from_secs(5)));

        let task = Task::new("Tarea lenta", "Espera mucho", Privacy::Open);
        let submitted = scheduler
            .submit(task, slow_agent)
            .await
            .expect("submit debe tener éxito");

        tokio::time::sleep(Duration::from_millis(20)).await;

        let cancelled = scheduler
            .cancel_run(&submitted.id)
            .await
            .expect("cancel_run debe tener éxito");
        assert_eq!(cancelled.status, RunStatus::Cancelled);

        let in_mem = scheduler
            .get_run(&submitted.id)
            .await
            .expect("run debe existir en memoria");
        assert_eq!(in_mem.status, RunStatus::Cancelled);

        let in_store = store
            .get_run(&submitted.id)
            .expect("leer run del store")
            .expect("run debe existir en store");
        assert_eq!(in_store.status, RunStatus::Cancelled);

        let not_found = scheduler.cancel_run("no-existe").await;
        assert!(matches!(not_found, Err(ForjaError::NotFound(_))));
    }

    #[tokio::test]
    async fn scheduler_creates_git_worktree_and_commits_changes() {
        let repo_dir = init_temp_repo();
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

        let mut config = SchedulerConfig::new(wt_dir.path());
        config.repo_path = Some(repo_dir.path().to_path_buf());
        config.base_branch = "main".to_string();

        let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");

        let agent: Arc<dyn Agent> = Arc::new(
            MockAgent::success("archivo creado").with_file("salida.txt", "hola desde forja\n"),
        );

        let task = Task::new("Crear archivo", "Escribe salida.txt", Privacy::Open);
        let submitted = scheduler.submit(task, agent).await.expect("submit");
        let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");

        assert_eq!(finished.status, RunStatus::Done);
        assert_eq!(finished.output.as_deref(), Some("archivo creado"));
        let wt_path = PathBuf::from(finished.worktree.as_ref().expect("worktree registrado"));
        assert!(wt_path.join("salida.txt").is_file());
        assert!(
            forja_git::status_clean(&wt_path).expect("status_clean en worktree"),
            "el worktree debe quedar limpio tras el commit automático"
        );
    }

    #[tokio::test]
    async fn scheduler_persists_failed_run_error() {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

        let config = SchedulerConfig::new(wt_dir.path());
        let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");

        let failing_agent: Arc<dyn Agent> = Arc::new(MockAgent::fail("fallo simulado"));
        let task = Task::new("Tarea con fallo", "Falla", Privacy::Open);
        let submitted = scheduler.submit(task, failing_agent).await.expect("submit");
        let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");

        assert_eq!(finished.status, RunStatus::Failed);
        assert!(finished
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("fallo simulado"));

        let stored = store
            .get_run(&submitted.id)
            .expect("get_run")
            .expect("debe existir en store");
        assert_eq!(stored.status, RunStatus::Failed);
        assert_eq!(stored.error, finished.error);
    }

    struct FlakyAgent {
        calls: AtomicU32,
        fail_times: u32,
    }

    impl FlakyAgent {
        fn new(fail_times: u32) -> Self {
            Self {
                calls: AtomicU32::new(0),
                fail_times,
            }
        }
    }

    impl Agent for FlakyAgent {
        fn kind(&self) -> AgentKind {
            AgentKind::Mock
        }

        fn run<'a>(&'a self, _ctx: &'a RunContext) -> AgentFuture<'a> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let fail_times = self.fail_times;

            Box::pin(async move {
                if attempt <= fail_times {
                    Err(ForjaError::Agent(format!("fallo en intento {attempt}")))
                } else {
                    Ok(AgentOutput {
                        text: format!("exito en intento {attempt}"),
                        raw_stdout: format!("exito en intento {attempt}"),
                        raw_stderr: String::new(),
                        exit_code: Some(0),
                        warnings: Vec::new(),
                    })
                }
            })
        }
    }

    #[tokio::test]
    async fn policy_denies_sensitive_with_agy_without_creating_worktree() {
        let repo_dir = init_temp_repo();
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

        let mut config = SchedulerConfig::new(wt_dir.path());
        config.repo_path = Some(repo_dir.path().to_path_buf());
        config.base_branch = "main".to_string();

        let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");
        let agent: Arc<dyn Agent> = Arc::new(AgyAgent::default());
        let task = Task::new("Dato confidencial", "Procesar secreto", Privacy::Sensitive);

        let err = scheduler
            .submit(task, agent)
            .await
            .expect_err("submit debe rechazar Sensitive + Agy");

        assert!(
            matches!(
                err,
                ForjaError::PolicyDenied {
                    privacy: Privacy::Sensitive,
                    agent: AgentKind::Agy
                }
            ),
            "error inesperado: {err:?}"
        );

        assert!(
            fs::read_dir(wt_dir.path())
                .expect("leer worktrees_dir")
                .next()
                .is_none(),
            "worktrees_dir debe permanecer vacío tras rechazo de política"
        );

        let branches_output = Command::new("git")
            .arg("-C")
            .arg(repo_dir.path())
            .args(["branch", "--list", "forja/*"])
            .output()
            .expect("listar ramas git");
        assert!(branches_output.status.success());
        assert!(
            String::from_utf8_lossy(&branches_output.stdout)
                .trim()
                .is_empty(),
            "no debe haberse creado ninguna rama forja/*"
        );

        assert!(scheduler.list_runs().await.is_empty());
        assert!(store.list_runs().expect("listar store").is_empty());
    }

    #[tokio::test]
    async fn retry_succeeds_on_third_attempt() {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

        let mut config = SchedulerConfig::new(wt_dir.path());
        config.max_retries = 2;
        config.retry_backoff = Duration::from_millis(10);

        let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");
        let agent: Arc<dyn Agent> = Arc::new(FlakyAgent::new(2));
        let task = Task::new("Tarea inestable", "Reintentar hasta éxito", Privacy::Open);

        let submitted = scheduler.submit(task, agent).await.expect("submit");
        let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");

        assert_eq!(finished.status, RunStatus::Done);
        assert_eq!(finished.attempts, 3);
        assert_eq!(finished.output.as_deref(), Some("exito en intento 3"));
        assert!(finished.error.is_none());

        let stored = store
            .get_run(&submitted.id)
            .expect("get_run")
            .expect("run en store");
        assert_eq!(stored.status, RunStatus::Done);
        assert_eq!(stored.attempts, 3);
    }

    #[tokio::test]
    async fn retry_exhausted_marks_failed() {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");

        let mut config = SchedulerConfig::new(wt_dir.path());
        config.max_retries = 2;
        config.retry_backoff = Duration::from_millis(10);

        let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");
        let agent: Arc<dyn Agent> = Arc::new(FlakyAgent::new(u32::MAX));
        let task = Task::new("Tarea siempre fallida", "Falla siempre", Privacy::Open);

        let submitted = scheduler.submit(task, agent).await.expect("submit");
        let finished = scheduler.wait_run(&submitted.id).await.expect("wait_run");

        assert_eq!(finished.status, RunStatus::Failed);
        assert_eq!(finished.attempts, 3);
        assert!(finished
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("fallo en intento 3"));

        let stored = store
            .get_run(&submitted.id)
            .expect("get_run")
            .expect("run en store");
        assert_eq!(stored.status, RunStatus::Failed);
        assert_eq!(stored.attempts, 3);
        assert_eq!(stored.error, finished.error);
    }

    #[tokio::test]
    async fn concurrency_dynamic_limit_updates_live() {
        let store_dir = TempDir::new().expect("store tempdir");
        let wt_dir = TempDir::new().expect("wt tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("store");

        let mut config = SchedulerConfig::new(wt_dir.path());
        config.max_concurrency = 1;
        let scheduler = Scheduler::new(store, config).expect("scheduler");
        assert_eq!(scheduler.max_concurrency(), 1);
        assert!(scheduler.set_max_concurrency(0).is_err());

        let active = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let agent: Arc<dyn Agent> = Arc::new(CountingAgent {
            delay: Duration::from_millis(80),
            active: Arc::clone(&active),
            max_observed: Arc::clone(&max_seen),
        });

        for i in 0..4 {
            scheduler
                .submit(
                    Task::new(format!("Dyn {i}"), "p", Privacy::Open),
                    Arc::clone(&agent),
                )
                .await
                .expect("submit");
        }

        // Subir límite en caliente a 3 mientras hay tareas en cola
        tokio::time::sleep(Duration::from_millis(15)).await;
        scheduler.set_max_concurrency(3).expect("subir a 3");
        assert_eq!(scheduler.max_concurrency(), 3);

        scheduler.wait_all().await;
        assert!(
            max_seen.load(Ordering::SeqCst) >= 2 && max_seen.load(Ordering::SeqCst) <= 3,
            "esperaba concurrencia dinámica entre 2 y 3, observada: {}",
            max_seen.load(Ordering::SeqCst)
        );
    }

    // ---- salida en vivo ----

    struct ScriptAgent {
        env: Vec<(String, String)>,
    }

    impl ScriptAgent {
        fn arc(env: &[(&str, &str)]) -> Arc<dyn Agent> {
            Arc::new(Self {
                env: env
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            })
        }
    }

    impl Agent for ScriptAgent {
        fn kind(&self) -> AgentKind {
            AgentKind::Mock
        }

        fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
            Box::pin(async move {
                let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../forja-agent/tests/fake_stream.sh");
                let argv = vec![script.to_string_lossy().into_owned()];
                let env: Vec<(&str, &str)> = self
                    .env
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
                Ok(AgentOutput {
                    text: res.stdout.trim().to_string(),
                    raw_stdout: res.stdout,
                    raw_stderr: res.stderr,
                    exit_code: res.exit_code,
                    warnings: Vec::new(),
                })
            })
        }
    }

    fn new_scheduler(max_concurrency: usize) -> (Scheduler, TempDir, TempDir) {
        let store_dir = TempDir::new().expect("store tempdir");
        let wt_dir = TempDir::new().expect("wt tempdir");
        let store = JsonlStore::new(store_dir.path()).expect("store");
        let mut config = SchedulerConfig::new(wt_dir.path());
        config.max_concurrency = max_concurrency;
        (
            Scheduler::new(store, config).expect("scheduler"),
            store_dir,
            wt_dir,
        )
    }

    fn open_task(name: &str) -> Task {
        Task::new(name, "prompt", Privacy::Open)
    }

    #[tokio::test]
    async fn output_streams_live_before_run_finishes() {
        let (sched, _a, _b) = new_scheduler(2);
        let agent = ScriptAgent::arc(&[("FAKE_STREAM_LINES", "4"), ("FAKE_STREAM_SLEEP", "0.15")]);
        let run = sched
            .submit(open_task("live"), agent)
            .await
            .expect("submit");

        let mut sub = sched.subscribe_output(&run.id, 0).await.expect("subscribe");
        let first = tokio::time::timeout(Duration::from_secs(5), sub.next_batch())
            .await
            .expect("sin timeout")
            .expect("primer lote");
        assert_eq!(first[0].text, "linea 1\n");
        assert_eq!(first[0].stream, "stdout");
        assert_eq!(first[0].seq, 0);
        let status = sched.get_run(&run.id).await.expect("run").status;
        assert_eq!(status, RunStatus::Running, "debe seguir corriendo");

        let mut texts: Vec<String> = first.into_iter().map(|c| c.text).collect();
        while let Some(batch) = sub.next_batch().await {
            texts.extend(batch.into_iter().map(|c| c.text));
        }
        assert_eq!(texts, ["linea 1\n", "linea 2\n", "linea 3\n", "linea 4\n"]);
        let done = sched.wait_run(&run.id).await.expect("wait");
        assert_eq!(done.status, RunStatus::Done);
    }

    #[tokio::test]
    async fn output_buffer_is_capped_and_since_reconnects() {
        let (sched, _a, _b) = new_scheduler(2);
        sched.set_output_limits(5, 1024 * 1024);
        let agent = ScriptAgent::arc(&[("FAKE_STREAM_LINES", "20"), ("FAKE_STREAM_SLEEP", "0")]);
        let run = sched.submit(open_task("cap"), agent).await.expect("submit");
        sched.wait_run(&run.id).await.expect("wait");

        let page = sched.output_since(&run.id, 0).await.expect("page");
        assert_eq!(page.chunks.len(), 5);
        assert_eq!(page.chunks[0].seq, 15);
        assert_eq!(page.chunks[0].text, "linea 16\n");
        assert_eq!(page.next_seq, 20);
        assert!(page.truncated);
        assert!(page.finished);

        let page = sched.output_since(&run.id, 18).await.expect("page");
        assert_eq!(
            page.chunks.iter().map(|c| c.seq).collect::<Vec<_>>(),
            [18, 19]
        );
        assert!(!page.truncated);
        let page = sched.output_since(&run.id, 20).await.expect("page");
        assert!(page.chunks.is_empty());
        assert_eq!(page.next_seq, 20);
    }

    #[tokio::test]
    async fn output_byte_cap_drops_oldest() {
        let (sched, _a, _b) = new_scheduler(1);
        sched.set_output_limits(2000, 40);
        let agent = ScriptAgent::arc(&[("FAKE_STREAM_LINES", "30"), ("FAKE_STREAM_SLEEP", "0")]);
        let run = sched
            .submit(open_task("bytes"), agent)
            .await
            .expect("submit");
        sched.wait_run(&run.id).await.expect("wait");
        let page = sched.output_since(&run.id, 0).await.expect("page");
        let bytes: usize = page.chunks.iter().map(|c| c.text.len()).sum();
        assert!(bytes <= 40, "bytes retenidos: {bytes}");
        assert_eq!(page.chunks.last().expect("último").text, "linea 30\n");
        assert!(page.truncated);
    }

    #[tokio::test]
    async fn two_runs_stream_in_parallel_without_mixing() {
        let (sched, _a, _b) = new_scheduler(2);
        let mk = |tag: &str| {
            ScriptAgent::arc(&[
                ("FAKE_STREAM_LINES", "5"),
                ("FAKE_STREAM_SLEEP", "0.2"),
                ("FAKE_STREAM_TAG", tag),
            ])
        };
        let a = sched.submit(open_task("A"), mk("alfa")).await.expect("a");
        let b = sched.submit(open_task("B"), mk("beta")).await.expect("b");

        // Ambos emiten algo mientras ninguno ha terminado.
        let mut overlapped = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let pa = sched.output_since(&a.id, 0).await.expect("pa");
            let pb = sched.output_since(&b.id, 0).await.expect("pb");
            if !pa.chunks.is_empty() && !pb.chunks.is_empty() && !pa.finished && !pb.finished {
                overlapped = true;
                break;
            }
        }
        assert!(overlapped, "los dos runs debieron transmitir a la vez");

        sched.wait_all().await;
        for (id, tag) in [(&a.id, "alfa"), (&b.id, "beta")] {
            let page = sched.output_since(id, 0).await.expect("page");
            let texts: Vec<String> = page.chunks.iter().map(|c| c.text.clone()).collect();
            let expected: Vec<String> = (1..=5).map(|i| format!("{tag} {i}\n")).collect();
            assert_eq!(texts, expected);
        }
    }

    #[tokio::test]
    async fn max_concurrency_still_applies_to_streaming_runs() {
        let (sched, _a, _b) = new_scheduler(1);
        let mk = |tag: &str| {
            ScriptAgent::arc(&[
                ("FAKE_STREAM_LINES", "3"),
                ("FAKE_STREAM_SLEEP", "0.15"),
                ("FAKE_STREAM_TAG", tag),
            ])
        };
        let a = sched.submit(open_task("A"), mk("uno")).await.expect("a");
        let b = sched.submit(open_task("B"), mk("dos")).await.expect("b");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pb = sched.output_since(&b.id, 0).await.expect("pb");
        assert!(pb.chunks.is_empty(), "B no debe arrancar con límite 1");
        assert!(!sched
            .output_since(&a.id, 0)
            .await
            .expect("pa")
            .chunks
            .is_empty());
        sched.wait_all().await;
    }

    #[tokio::test]
    async fn finished_run_replays_buffer_and_closes() {
        let (sched, _a, _b) = new_scheduler(2);
        let run = sched
            .submit(
                open_task("fin"),
                ScriptAgent::arc(&[("FAKE_STREAM_LINES", "2"), ("FAKE_STREAM_SLEEP", "0")]),
            )
            .await
            .expect("submit");
        sched.wait_run(&run.id).await.expect("wait");

        let mut sub = sched.subscribe_output(&run.id, 0).await.expect("sub");
        let batch = sub.next_batch().await.expect("backlog");
        assert_eq!(batch.len(), 2);
        assert!(sub.next_batch().await.is_none(), "debe cerrar");
    }

    #[tokio::test]
    async fn non_streaming_agent_emits_output_at_end() {
        let (sched, _a, _b) = new_scheduler(2);
        let run = sched
            .submit(
                open_task("mock"),
                Arc::new(MockAgent::success("hola\nmundo")),
            )
            .await
            .expect("submit");
        sched.wait_run(&run.id).await.expect("wait");
        let page = sched.output_since(&run.id, 0).await.expect("page");
        let all: String = page.chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(all.contains("hola\nmundo"), "salida: {all:?}");
        assert!(page.finished);
    }

    #[tokio::test]
    async fn output_is_redacted_before_emission() {
        let (sched, _a, _b) = new_scheduler(2);
        let run = sched
            .submit(
                open_task("secret"),
                ScriptAgent::arc(&[
                    ("FAKE_STREAM_LINES", "1"),
                    ("FAKE_STREAM_SLEEP", "0"),
                    ("FAKE_STREAM_SECRET", "sk-live1234567890abcdef"),
                ]),
            )
            .await
            .expect("submit");
        sched.wait_run(&run.id).await.expect("wait");
        let page = sched.output_since(&run.id, 0).await.expect("page");
        let all: String = page.chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(all.contains("clave ***"), "salida: {all:?}");
        assert!(!all.contains("sk-live"));
    }

    #[tokio::test]
    async fn output_unknown_or_invalid_id_is_not_found() {
        let (sched, _a, _b) = new_scheduler(1);
        for id in ["no-existe", "../etc/passwd", "", "a b"] {
            let err = sched.output_since(id, 0).await.expect_err("debe fallar");
            assert!(matches!(err, ForjaError::NotFound(_)), "{id}: {err:?}");
            assert!(sched.subscribe_output(id, 0).await.is_err());
        }
    }
}
