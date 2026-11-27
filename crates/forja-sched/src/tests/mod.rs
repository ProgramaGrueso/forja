mod concurrency;
mod execution;
mod streaming;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forja_agent::{Agent, AgentFuture, AgentOutput, RunContext};
use forja_core::{AgentKind, ForjaError, Privacy, Task};
use forja_store::JsonlStore;
use tempfile::TempDir;

use super::{Scheduler, SchedulerConfig};

pub(super) struct CountingAgent {
    pub(super) active: Arc<AtomicUsize>,
    pub(super) max_observed: Arc<AtomicUsize>,
    pub(super) delay: Duration,
}

impl CountingAgent {
    pub(super) fn new(
        active: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
        delay: Duration,
    ) -> Self {
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

pub(super) struct FlakyAgent {
    calls: AtomicU32,
    fail_times: u32,
}

impl FlakyAgent {
    pub(super) fn new(fail_times: u32) -> Self {
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

pub(super) struct ScriptAgent {
    env: Vec<(String, String)>,
}

impl ScriptAgent {
    pub(super) fn arc(env: &[(&str, &str)]) -> Arc<dyn Agent> {
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

pub(super) fn run_git(repo: &Path, args: &[&str]) {
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

pub(super) fn init_temp_repo() -> TempDir {
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

pub(super) fn new_scheduler(max_concurrency: usize) -> (Scheduler, TempDir, TempDir) {
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

pub(super) fn open_task(name: &str) -> Task {
    Task::new(name, "prompt", Privacy::Open)
}
