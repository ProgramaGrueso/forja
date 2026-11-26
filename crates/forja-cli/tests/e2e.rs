use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use forja_agent::MockAgent;
use forja_core::{Privacy, RunStatus, Task};
use forja_sched::{Scheduler, SchedulerConfig};
use forja_store::JsonlStore;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("ejecutar git en test e2e");
    assert!(
        output.status.success(),
        "git {:?} falló:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_temp_repo() -> TempDir {
    let dir = TempDir::new().expect("crear repo temporal");
    let repo = dir.path();
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.name", "Forja E2E"]);
    git(repo, &["config", "user.email", "forja-e2e@example.com"]);
    fs::write(repo.join("README.md"), "# Repo E2E\n").expect("escribir README");
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-m", "commit inicial"]);
    dir
}

#[tokio::test]
async fn e2e_three_parallel_mock_tasks_create_worktrees_branches_commits_and_store() {
    let repo_dir = init_temp_repo();
    let store_dir = TempDir::new().expect("crear directorio de store");
    let worktrees_dir = TempDir::new().expect("crear directorio de worktrees");

    let store = JsonlStore::new(store_dir.path()).expect("crear JsonlStore");
    let mut config = SchedulerConfig::new(worktrees_dir.path());
    config.max_concurrency = 3;
    config.repo_path = Some(repo_dir.path().to_path_buf());
    config.base_branch = "main".to_string();

    let scheduler = Scheduler::new(store.clone(), config).expect("crear Scheduler");

    let specs = [
        (
            "Tarea Alpha",
            "Generar artefacto alpha",
            "alpha.txt",
            "contenido alpha\n",
        ),
        (
            "Tarea Beta",
            "Generar artefacto beta",
            "beta.txt",
            "contenido beta\n",
        ),
        (
            "Tarea Gamma",
            "Generar artefacto gamma",
            "gamma.txt",
            "contenido gamma\n",
        ),
    ];

    let mut run_ids = Vec::new();
    for (titulo, prompt, filename, content) in specs {
        let task = Task::new(titulo, prompt, Privacy::Open);
        let agent = Arc::new(
            MockAgent::success(format!("ok: {titulo}"))
                .with_delay(Duration::from_millis(30))
                .with_file(filename, content),
        );
        let run = scheduler
            .submit(task, agent)
            .await
            .expect("encolar tarea en e2e");
        run_ids.push((run.id, filename));
    }

    scheduler.wait_all().await;

    let persisted = store.list_runs().expect("leer ejecuciones del store");
    assert_eq!(persisted.len(), 3, "deben persistirse las 3 ejecuciones");

    for (run_id, filename) in &run_ids {
        let rec = store
            .get_run(run_id)
            .expect("consultar run en store")
            .expect("el run debe existir en el store");

        assert_eq!(
            rec.status,
            RunStatus::Done,
            "el run {run_id} debe terminar en Done (error: {:?})",
            rec.error
        );

        let branch = rec
            .branch
            .as_deref()
            .expect("el run debe tener una rama asignada");
        let worktree = rec
            .worktree
            .as_deref()
            .expect("el run debe tener un worktree asignado");

        // Verificar que la rama existe en el repositorio principal.
        let ref_name = format!("refs/heads/{branch}");
        let rev = git(repo_dir.path(), &["rev-parse", "--verify", &ref_name]);
        assert!(!rev.is_empty(), "la rama {branch} debe existir");

        // Verificar que la rama tiene exactamente 1 commit nuevo sobre main.
        let ahead = git(
            repo_dir.path(),
            &["rev-list", "--count", &format!("main..{branch}")],
        );
        assert_eq!(
            ahead, "1",
            "la rama {branch} debe tener 1 commit por la tarea"
        );

        // Verificar que el worktree existe, contiene el archivo creado y quedó limpio tras el commit.
        let wt_path = Path::new(worktree);
        assert!(wt_path.is_dir(), "el worktree {worktree} debe existir");
        assert!(
            wt_path.join(filename).is_file(),
            "el archivo {filename} debe existir en {worktree}"
        );
        let porcelain = git(wt_path, &["status", "--porcelain"]);
        assert!(
            porcelain.is_empty(),
            "el worktree {worktree} debe quedar limpio tras el commit"
        );
    }
}
