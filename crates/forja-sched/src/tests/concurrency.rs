use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forja_agent::{Agent, MockAgent};
use forja_core::{ForjaError, Privacy, RunStatus, Task};
use forja_store::JsonlStore;
use tempfile::TempDir;

use super::CountingAgent;
use crate::{Scheduler, SchedulerConfig};

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
