use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use forja_agent::{Agent, RunContext};
use forja_core::{ForjaError, Result, RunRecord, RunStatus, Task};
use forja_store::JsonlStore;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

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
            max_retries: 0,
            retry_backoff: Duration::from_millis(50),
        }
    }
}

struct SchedulerInner {
    config: SchedulerConfig,
    store: JsonlStore,
    semaphore: Arc<Semaphore>,
    runs: RwLock<HashMap<String, RunRecord>>,
    handles: Mutex<HashMap<String, JoinHandle<()>>>,
    store_lock: Mutex<()>,
    git_lock: Mutex<()>,
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

        let semaphore = Arc::new(Semaphore::new(config.max_concurrency));

        Ok(Self {
            inner: Arc::new(SchedulerInner {
                config,
                store,
                semaphore,
                runs: RwLock::new(runs_map),
                handles: Mutex::new(HashMap::new()),
                store_lock: Mutex::new(()),
                git_lock: Mutex::new(()),
            }),
        })
    }

    pub async fn submit(&self, task: Task, agent: Arc<dyn Agent>) -> Result<RunRecord> {
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
            Ok(record)
        } else {
            Err(ForjaError::NotFound(run_id.to_string()))
        }
    }
}

async fn run_task(inner: Arc<SchedulerInner>, run_id: String, agent: Arc<dyn Agent>) {
    let _permit = match Arc::clone(&inner.semaphore).acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return,
    };

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
            record.attempts = record.attempts.saturating_add(1);
            record.branch = branch.clone();
            record.worktree = Some(wt_path.to_string_lossy().into_owned());
            record.updated_at_ms = now_ms();
            record.clone()
        };
        let _ = inner.store.append_run(&maybe_record);
        maybe_record.task
    };

    let execution_result =
        execute_in_worktree(&inner, &task, &agent, &wt_path, branch.as_deref()).await;

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
    }

    let mut handles = inner.handles.lock().await;
    handles.remove(&run_id);
}

async fn execute_in_worktree(
    inner: &SchedulerInner,
    task: &Task,
    agent: &Arc<dyn Agent>,
    wt_path: &Path,
    branch: Option<&str>,
) -> Result<String> {
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

    let ctx = RunContext {
        task: task.clone(),
        worktree: wt_path.to_path_buf(),
        timeout_secs: inner.config.timeout_secs,
    };

    let agent_output = agent.run(&ctx).await?;

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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use forja_agent::{Agent, AgentFuture, AgentOutput, MockAgent, RunContext};
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
}
