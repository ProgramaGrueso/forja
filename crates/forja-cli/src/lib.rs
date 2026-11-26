use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use forja_agent::{Agent, AgyAgent, HermesAgent, LocalAgent, MockAgent, OpencodeAgent};
use forja_core::{ForjaError, Privacy, Result, RunRecord, RunStatus, Task};
use forja_sched::{Scheduler, SchedulerConfig};
use forja_server::AppState;
use forja_store::JsonlStore;

#[derive(Debug, Clone, Parser)]
#[command(name = "forja", about = "Orquestador local de tareas y agentes")]
pub struct Cli {
    #[arg(long, global = true, default_value = ".forja/store")]
    pub store_dir: PathBuf,

    #[arg(long, global = true, default_value = ".forja/worktrees")]
    pub worktrees_dir: PathBuf,

    #[arg(long, global = true)]
    pub repo: Option<PathBuf>,

    #[arg(long, global = true, default_value = "main")]
    pub base_branch: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Ejecuta una tarea con un agente y espera su resultado.
    Run {
        /// Agente que ejecuta la tarea: mock, agy, opencode, hermes o local.
        #[arg(long, value_enum)]
        agent: CliAgent,

        #[arg(long, value_enum)]
        privacy: CliPrivacy,

        #[arg(long)]
        title: Option<String>,

        prompt: String,
    },

    /// Lista las ejecuciones registradas en el store.
    List,

    /// Inicia el servidor HTTP local de Forja.
    Serve {
        #[arg(long, default_value_t = 7777)]
        port: u16,

        /// Token Bearer opcional para proteger las rutas /api/*, /tasks, /runs y /events.
        #[arg(long)]
        auth_token: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliAgent {
    Mock,
    Agy,
    Opencode,
    Hermes,
    Local,
}

impl CliAgent {
    fn build_agent(self) -> Arc<dyn Agent> {
        match self {
            Self::Mock => Arc::new(MockAgent::default()),
            Self::Agy => Arc::new(AgyAgent::default()),
            Self::Opencode => Arc::new(OpencodeAgent::default()),
            Self::Hermes => Arc::new(HermesAgent::default()),
            Self::Local => Arc::new(LocalAgent::default()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliPrivacy {
    Open,
    Sensitive,
}

impl From<CliPrivacy> for Privacy {
    fn from(value: CliPrivacy) -> Self {
        match value {
            CliPrivacy::Open => Self::Open,
            CliPrivacy::Sensitive => Self::Sensitive,
        }
    }
}

pub async fn execute(cli: Cli) -> Result<()> {
    let mut stdout = std::io::stdout();
    execute_with_writer(cli, &mut stdout).await
}

pub async fn execute_with_writer<W: Write>(cli: Cli, out: &mut W) -> Result<()> {
    match cli.command {
        Command::Run {
            agent,
            privacy,
            title,
            prompt,
        } => {
            let scheduler = build_scheduler(
                &cli.store_dir,
                &cli.worktrees_dir,
                cli.repo,
                cli.base_branch,
            )?;
            let titulo = title.unwrap_or_else(|| derive_title(&prompt));
            let task = Task::new(titulo, prompt, privacy.into());

            let submitted = scheduler.submit(task, agent.build_agent()).await?;
            let finished = scheduler.wait_run(&submitted.id).await?;

            writeln!(out, "id: {}", finished.id)?;
            writeln!(out, "estado: {}", finished.status)?;
            if let Some(output) = &finished.output {
                writeln!(out, "salida: {output}")?;
            }
            if let Some(err) = &finished.error {
                writeln!(out, "error: {err}")?;
            }

            if finished.status == RunStatus::Failed {
                let msg = finished
                    .error
                    .unwrap_or_else(|| "ejecución fallida sin detalle".to_string());
                return Err(ForjaError::Agent(msg));
            }

            Ok(())
        }
        Command::List => {
            if !cli.store_dir.exists() {
                writeln!(out, "No hay ejecuciones registradas.")?;
                return Ok(());
            }

            let store = JsonlStore::new(&cli.store_dir)?;
            let runs = store.list_runs()?;
            write_runs_table(&runs, out)?;
            Ok(())
        }
        Command::Serve { port, auth_token } => {
            let settings_path = cli.store_dir.join("settings.json");
            let persisted = forja_server::settings::load_settings(&settings_path)
                .ok()
                .flatten();

            let max_concurrency = persisted.as_ref().map(|s| s.max_concurrency).unwrap_or(2);

            let quota_path = persisted
                .as_ref()
                .map(|s| s.quota_path.clone())
                .unwrap_or_else(|| {
                    forja_server::quota::default_manual_path()
                        .to_string_lossy()
                        .into_owned()
                });

            let hide_mascot = persisted.as_ref().is_some_and(|s| s.hide_mascot);

            let store = JsonlStore::new(&cli.store_dir)?;
            let mut sched_cfg = SchedulerConfig::new(&cli.worktrees_dir);
            sched_cfg.max_concurrency = max_concurrency;
            sched_cfg.repo_path = cli.repo;
            sched_cfg.base_branch = cli.base_branch;
            let scheduler = Scheduler::new(store, sched_cfg)?;

            let settings = forja_server::settings::Settings {
                max_concurrency,
                store_dir: cli.store_dir.to_string_lossy().into_owned(),
                port,
                quota_path,
                hide_mascot,
            };

            let mut state = AppState::new(scheduler)
                .with_settings(settings)
                .with_settings_path(settings_path);
            if let Some(token) = auth_token {
                state = state.with_auth_token(token);
            }
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            writeln!(out, "http://127.0.0.1:{port}")?;
            out.flush()?;
            forja_server::serve(addr, state).await
        }
    }
}

fn build_scheduler(
    store_dir: &PathBuf,
    worktrees_dir: &PathBuf,
    repo: Option<PathBuf>,
    base_branch: String,
) -> Result<Scheduler> {
    let store = JsonlStore::new(store_dir)?;
    let mut config = SchedulerConfig::new(worktrees_dir);
    config.repo_path = repo;
    config.base_branch = base_branch;
    Scheduler::new(store, config)
}

fn derive_title(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return "Sin título".to_string();
    }
    const MAX_LEN: usize = 60;
    if first_line.chars().count() <= MAX_LEN {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(MAX_LEN).collect();
        format!("{truncated}...")
    }
}

fn write_runs_table<W: Write>(runs: &[RunRecord], out: &mut W) -> Result<()> {
    if runs.is_empty() {
        writeln!(out, "No hay ejecuciones registradas.")?;
        return Ok(());
    }

    writeln!(
        out,
        "{:<36}  {:<9}  {:<6}  {:<10}  TITULO",
        "ID", "ESTADO", "AGENTE", "PRIVACIDAD"
    )?;
    for run in runs {
        writeln!(
            out,
            "{:<36}  {:<9}  {:<6}  {:<10}  {}",
            run.id, run.status, run.agent, run.task.privacidad, run.task.titulo
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use forja_core::{AgentKind, ForjaError, Privacy};
    use tempfile::TempDir;

    use super::{execute_with_writer, Cli, CliAgent, CliPrivacy, Command};

    #[test]
    fn parse_run_subcommand() {
        let cli = Cli::try_parse_from([
            "forja",
            "--store-dir",
            "/tmp/store",
            "--worktrees-dir",
            "/tmp/wt",
            "--repo",
            "/tmp/repo",
            "--base-branch",
            "develop",
            "run",
            "--agent",
            "mock",
            "--privacy",
            "open",
            "--title",
            "Mi tarea",
            "hola mundo",
        ])
        .expect("parsear run");

        assert_eq!(cli.store_dir.to_str(), Some("/tmp/store"));
        assert_eq!(cli.worktrees_dir.to_str(), Some("/tmp/wt"));
        assert_eq!(
            cli.repo.as_deref().and_then(|p| p.to_str()),
            Some("/tmp/repo")
        );
        assert_eq!(cli.base_branch, "develop");

        match cli.command {
            Command::Run {
                agent,
                privacy,
                title,
                prompt,
            } => {
                assert_eq!(agent, CliAgent::Mock);
                assert_eq!(privacy, CliPrivacy::Open);
                assert_eq!(title.as_deref(), Some("Mi tarea"));
                assert_eq!(prompt, "hola mundo");
            }
            other => panic!("subcomando inesperado: {other:?}"),
        }
    }

    #[test]
    fn parse_list_subcommand() {
        let cli = Cli::try_parse_from(["forja", "list"]).expect("parsear list");
        assert_eq!(cli.store_dir.to_str(), Some(".forja/store"));
        assert_eq!(cli.worktrees_dir.to_str(), Some(".forja/worktrees"));
        assert!(cli.repo.is_none());
        assert_eq!(cli.base_branch, "main");
        assert!(matches!(cli.command, Command::List));
    }

    #[test]
    fn parse_serve_subcommand() {
        let cli = Cli::try_parse_from(["forja", "serve", "--port", "7777"]).expect("parsear serve");
        match cli.command {
            Command::Serve { port, auth_token } => {
                assert_eq!(port, 7777);
                assert!(auth_token.is_none());
            }
            other => panic!("subcomando inesperado: {other:?}"),
        }
    }

    #[test]
    fn parse_run_accepts_opencode_hermes_and_local() {
        for (raw, expected) in [
            ("opencode", CliAgent::Opencode),
            ("hermes", CliAgent::Hermes),
            ("local", CliAgent::Local),
        ] {
            let cli = Cli::try_parse_from([
                "forja",
                "run",
                "--agent",
                raw,
                "--privacy",
                "open",
                "prompt",
            ])
            .expect("parsear run con agente nuevo");

            match cli.command {
                Command::Run { agent, .. } => assert_eq!(agent, expected),
                other => panic!("subcomando inesperado: {other:?}"),
            }
        }
    }

    #[test]
    fn run_help_mentions_all_five_agents() {
        use clap::CommandFactory;

        let mut cmd = Cli::command();
        let run = cmd.find_subcommand_mut("run").expect("subcomando run");
        let help = run.render_help().to_string();
        for name in ["mock", "agy", "opencode", "hermes", "local"] {
            assert!(
                help.contains(name),
                "run --help debe mencionar {name}; ayuda real:\n{help}"
            );
        }
    }

    #[tokio::test]
    async fn list_with_empty_and_missing_store_succeeds_without_creating_dir() {
        let base = TempDir::new().expect("crear tempdir");
        let missing_store = base.path().join("no-existe");

        let cli = Cli::try_parse_from([
            "forja",
            "--store-dir",
            missing_store.to_str().expect("ruta utf8"),
            "list",
        ])
        .expect("parsear list");

        let mut buf = Vec::new();
        execute_with_writer(cli, &mut buf)
            .await
            .expect("list debe tener éxito");

        let output = String::from_utf8(buf).expect("salida utf8");
        assert!(output.contains("No hay ejecuciones registradas."));
        assert!(
            !missing_store.exists(),
            "list no debe crear store_dir si no existe"
        );
    }

    #[tokio::test]
    async fn run_mock_open_persists_and_appears_in_list() {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");

        let run_cli = Cli::try_parse_from([
            "forja",
            "--store-dir",
            store_dir.path().to_str().expect("store utf8"),
            "--worktrees-dir",
            wt_dir.path().to_str().expect("wt utf8"),
            "run",
            "--agent",
            "mock",
            "--privacy",
            "open",
            "hola",
        ])
        .expect("parsear run");

        let mut run_buf = Vec::new();
        execute_with_writer(run_cli, &mut run_buf)
            .await
            .expect("run mock debe tener éxito");

        let run_output = String::from_utf8(run_buf).expect("salida utf8");
        assert!(run_output.contains("estado: done"));

        let list_cli = Cli::try_parse_from([
            "forja",
            "--store-dir",
            store_dir.path().to_str().expect("store utf8"),
            "list",
        ])
        .expect("parsear list");

        let mut list_buf = Vec::new();
        execute_with_writer(list_cli, &mut list_buf)
            .await
            .expect("list debe tener éxito");

        let list_output = String::from_utf8(list_buf).expect("salida utf8");
        assert!(list_output.contains("ID"));
        assert!(list_output.contains("done"));
        assert!(list_output.contains("mock"));
        assert!(list_output.contains("open"));
        assert!(list_output.contains("hola"));
    }

    #[tokio::test]
    async fn run_agy_sensitive_is_rejected_with_policy_denied() {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");

        let cli = Cli::try_parse_from([
            "forja",
            "--store-dir",
            store_dir.path().to_str().expect("store utf8"),
            "--worktrees-dir",
            wt_dir.path().to_str().expect("wt utf8"),
            "run",
            "--agent",
            "agy",
            "--privacy",
            "sensitive",
            "secreto",
        ])
        .expect("parsear run");

        let mut buf = Vec::new();
        let err = execute_with_writer(cli, &mut buf)
            .await
            .expect_err("agy + sensitive debe ser rechazado");

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
    }
}
