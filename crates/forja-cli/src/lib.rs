use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

mod friendly;
pub mod init;

use clap::{Parser, Subcommand, ValueEnum};
use forja_agent::Agent;
use forja_core::{AgentKind, ForjaError, Privacy, Result, RunRecord, RunStatus, Task};
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

    /// Rama base de los worktrees. Si no se indica, se usa la rama actual de `--repo` (o `main`).
    #[arg(long, global = true)]
    pub base_branch: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Ejecuta una tarea con un agente y espera su resultado.
    Run {
        /// Agente que ejecuta la tarea: mock, agy, claude, opencode, hermes o local.
        #[arg(long, value_enum)]
        agent: CliAgent,

        #[arg(long, value_enum)]
        privacy: CliPrivacy,

        #[arg(long)]
        title: Option<String>,

        /// Modelo solo para esta ejecución (prioridad sobre Ajustes y el default).
        #[arg(long)]
        model: Option<String>,

        /// Esfuerzo de razonamiento solo para esta ejecución (claude/agy: low..max;
        /// hermes: none..ultra; opencode: variante libre). Prioridad sobre Ajustes.
        #[arg(long)]
        effort: Option<String>,

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

        /// Abre el panel en el navegador al arrancar.
        #[arg(long)]
        open: bool,
    },

    /// Revisa el entorno (git, agentes instalados, repo, puerto) y dice qué falta.
    Doctor {
        #[arg(long, default_value_t = 7777)]
        port: u16,

        /// Tras el diagnóstico, lanza el asistente de configuración (`forja init`).
        #[arg(long)]
        fix: bool,
    },

    /// Asistente de primer arranque: detecta agentes, elige orquestador y modelos y
    /// escribe `<store_dir>/settings.json` (sin pisar lo que ya tengas).
    Init {
        /// No pregunta nada: acepta los valores por defecto.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliAgent {
    Mock,
    Agy,
    Claude,
    Opencode,
    Hermes,
    Local,
}

impl CliAgent {
    fn kind(self) -> AgentKind {
        match self {
            Self::Mock => AgentKind::Mock,
            Self::Agy => AgentKind::Agy,
            Self::Claude => AgentKind::Claude,
            Self::Opencode => AgentKind::Opencode,
            Self::Hermes => AgentKind::Hermes,
            Self::Local => AgentKind::Local,
        }
    }

    /// Modelo: `--model` > ajustes persistidos > default en código.
    /// Esfuerzo: `--effort` > ajustes persistidos > ninguno (no se pasa flag).
    fn build_agent(
        self,
        model: Option<&str>,
        agent_models: &BTreeMap<String, String>,
        effort: Option<&str>,
        agent_effort: &BTreeMap<String, String>,
    ) -> Result<Arc<dyn Agent>> {
        let kind = self.kind();
        let model = match model.map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) => {
                if !forja_agent::MODEL_AGENTS.contains(&kind) {
                    return Err(ForjaError::InvalidInput(format!(
                        "el agente {kind} no admite --model"
                    )));
                }
                Some(forja_agent::validate_model(m)?)
            }
            None => None,
        };
        let effort = match effort.map(str::trim).filter(|e| !e.is_empty()) {
            Some(e) => Some(forja_agent::validate_effort(kind, e)?),
            None => None,
        };
        let built = forja_agent::build_agent(kind, model.as_deref(), agent_models);
        match forja_agent::resolve_effort(kind, effort.as_deref(), agent_effort) {
            // Un valor inválido guardado en ajustes también se rechaza (no se pasa a un CLI).
            Some(e) => {
                let e = forja_agent::validate_effort(kind, &e)?;
                built.with_effort(&e).ok_or_else(|| {
                    ForjaError::InvalidInput(format!("el agente {kind} no admite --effort"))
                })
            }
            None => Ok(built),
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

pub async fn execute_with_writer<W: Write>(mut cli: Cli, out: &mut W) -> Result<()> {
    if matches!(cli.command, Command::Run { .. } | Command::Serve { .. }) {
        if cli.repo.is_none() {
            cli.repo = friendly::detect_repo();
        }
        if cli.base_branch.is_none() {
            cli.base_branch = cli.repo.as_deref().and_then(friendly::current_branch);
        }
    }
    let base_branch = cli
        .base_branch
        .clone()
        .unwrap_or_else(|| "main".to_string());
    match cli.command {
        Command::Run {
            agent,
            privacy,
            title,
            model,
            effort,
            prompt,
        } => {
            let (agent_models, agent_effort) = load_agent_settings(&cli.store_dir);
            let built = agent.build_agent(
                model.as_deref(),
                &agent_models,
                effort.as_deref(),
                &agent_effort,
            )?;
            let scheduler =
                build_scheduler(&cli.store_dir, &cli.worktrees_dir, cli.repo, base_branch)?;
            let titulo = title.unwrap_or_else(|| derive_title(&prompt));
            let task = Task::new(titulo, prompt, privacy.into());

            let submitted = scheduler.submit(task, built).await?;
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
        Command::Doctor { port, fix } => {
            friendly::doctor(out, cli.repo.as_deref(), port, &cli.store_dir)?;
            if fix {
                writeln!(out)?;
                let stdin = std::io::stdin();
                init::run_init(
                    &mut stdin.lock(),
                    out,
                    &cli.store_dir,
                    false,
                    &init::InitEnv::detect(),
                )?;
            }
            Ok(())
        }
        Command::Init { yes } => {
            let stdin = std::io::stdin();
            init::run_init(
                &mut stdin.lock(),
                out,
                &cli.store_dir,
                yes,
                &init::InitEnv::detect(),
            )?;
            Ok(())
        }
        Command::Serve {
            port,
            auth_token,
            open,
        } => {
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
            let agent_effort = persisted
                .as_ref()
                .map(|s| s.agent_effort.clone())
                .unwrap_or_default();
            let agent_models = persisted
                .as_ref()
                .map(|s| s.agent_models.clone())
                .unwrap_or_default();
            let orchestrator_profile = persisted
                .as_ref()
                .and_then(|s| s.orchestrator_profile.clone());
            let orchestrator_agent = persisted
                .as_ref()
                .and_then(|s| s.orchestrator_agent.clone());
            let hidden_profiles = persisted
                .as_ref()
                .map(|s| s.hidden_profiles.clone())
                .unwrap_or_default();

            let store = JsonlStore::new(&cli.store_dir)?;
            let mut sched_cfg = SchedulerConfig::new(&cli.worktrees_dir);
            sched_cfg.max_concurrency = max_concurrency;
            let sched_repo = cli.repo.clone();
            sched_cfg.repo_path = cli.repo;
            sched_cfg.base_branch = base_branch.clone();
            let scheduler = Scheduler::new(store, sched_cfg)?;

            let settings = forja_server::settings::Settings {
                max_concurrency,
                store_dir: cli.store_dir.to_string_lossy().into_owned(),
                port,
                quota_path,
                hide_mascot,
                agent_models,
                agent_effort,
                orchestrator_profile,
                orchestrator_agent,
                hidden_profiles,
            };

            let mut state = AppState::new(scheduler)
                .with_settings(settings)
                .with_settings_path(settings_path);
            if let Some(token) = auth_token {
                state = state.with_auth_token(token);
            }
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            let url = format!("http://127.0.0.1:{port}");
            writeln!(out, "Forja listo en {url}  (Ctrl+C para salir)")?;
            match &sched_repo {
                Some(r) => writeln!(out, "Repo: {} (rama base: {base_branch})", r.display())?,
                None => writeln!(
                    out,
                    "Sin repo git: las tareas no crean worktrees. Usa --repo o ejecuta Forja dentro de un repo."
                )?,
            }
            out.flush()?;
            if open {
                friendly::open_browser(&url);
            }
            forja_server::serve(addr, state).await
        }
    }
}

/// Modelos y esfuerzos por agente de `settings.json` (vacíos si no existe o es inválido).
fn load_agent_settings(
    store_dir: &std::path::Path,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    forja_server::settings::load_settings(&store_dir.join("settings.json"))
        .ok()
        .flatten()
        .map(|s| (s.agent_models, s.agent_effort))
        .unwrap_or_default()
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
    fn run_model_flag_parses_and_resolves_with_priority() {
        use std::collections::BTreeMap;
        let cli = Cli::try_parse_from([
            "forja",
            "run",
            "--agent",
            "claude",
            "--privacy",
            "open",
            "--model",
            "opus",
            "hola",
        ])
        .expect("parsear --model");
        let Command::Run { model, agent, .. } = cli.command else {
            panic!("esperaba run");
        };
        assert_eq!(model.as_deref(), Some("opus"));

        let settings: BTreeMap<String, String> =
            [("claude".to_string(), "haiku".to_string())].into();
        let none: BTreeMap<String, String> = BTreeMap::new();
        // --model > ajustes > default
        assert_eq!(
            agent
                .build_agent(Some("opus"), &settings, None, &none)
                .unwrap()
                .model()
                .as_deref(),
            Some("opus")
        );
        assert_eq!(
            agent
                .build_agent(None, &settings, None, &none)
                .unwrap()
                .model()
                .as_deref(),
            Some("haiku")
        );
        assert_eq!(
            CliAgent::Local
                .build_agent(None, &settings, None, &none)
                .unwrap()
                .model()
                .as_deref(),
            Some("moe")
        );
        // Mock no admite --model; hermes sí (opcional); modelos inválidos se rechazan.
        assert!(CliAgent::Mock
            .build_agent(Some("x"), &settings, None, &none)
            .is_err());
        assert!(CliAgent::Hermes
            .build_agent(Some("x"), &settings, None, &none)
            .is_ok());
        assert!(agent
            .build_agent(Some("--bad"), &settings, None, &none)
            .is_err());
    }

    #[test]
    fn run_effort_flag_resolves_with_priority_and_validates_per_family() {
        use std::collections::BTreeMap;
        let cli = Cli::try_parse_from([
            "forja",
            "run",
            "--agent",
            "claude",
            "--privacy",
            "open",
            "--effort",
            "max",
            "hola",
        ])
        .expect("parsear --effort");
        let Command::Run { effort, .. } = cli.command else {
            panic!("esperaba run");
        };
        assert_eq!(effort.as_deref(), Some("max"));

        let none: BTreeMap<String, String> = BTreeMap::new();
        let eff: BTreeMap<String, String> = [
            ("claude".to_string(), "low".to_string()),
            ("hermes".to_string(), "ultra".to_string()),
        ]
        .into();
        let e =
            |a: CliAgent, o: Option<&str>| a.build_agent(None, &none, o, &eff).map(|b| b.effort());
        // --effort > ajustes > ninguno
        assert_eq!(
            e(CliAgent::Claude, Some("max")).unwrap().as_deref(),
            Some("max")
        );
        assert_eq!(e(CliAgent::Claude, None).unwrap().as_deref(), Some("low"));
        assert_eq!(e(CliAgent::Hermes, None).unwrap().as_deref(), Some("ultra"));
        assert_eq!(e(CliAgent::Agy, None).unwrap(), None);
        // Cada familia con su conjunto; local/mock lo rechazan con mensaje claro.
        assert!(e(CliAgent::Claude, Some("ultra")).is_err());
        assert!(e(CliAgent::Hermes, Some("ultra")).is_ok());
        let err = e(CliAgent::Local, Some("high")).err().unwrap().to_string();
        assert!(err.contains("local no admite esfuerzo"), "{err}");
        assert!(e(CliAgent::Mock, Some("high")).is_err());
    }

    #[test]
    fn parse_init_and_doctor_fix() {
        let cli = Cli::try_parse_from(["forja", "init", "--yes"]).expect("init --yes");
        assert!(matches!(cli.command, Command::Init { yes: true }));
        let cli = Cli::try_parse_from(["forja", "init"]).expect("init");
        assert!(matches!(cli.command, Command::Init { yes: false }));
        let cli = Cli::try_parse_from(["forja", "doctor", "--fix"]).expect("doctor --fix");
        assert!(matches!(cli.command, Command::Doctor { fix: true, .. }));
        let cli = Cli::try_parse_from(["forja", "doctor"]).expect("doctor");
        assert!(matches!(cli.command, Command::Doctor { fix: false, .. }));
    }

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
        assert_eq!(cli.base_branch.as_deref(), Some("develop"));

        match cli.command {
            Command::Run {
                agent,
                privacy,
                title,
                prompt,
                ..
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
        assert!(cli.base_branch.is_none());
        assert!(matches!(cli.command, Command::List));
    }

    #[test]
    fn parse_serve_subcommand() {
        let cli = Cli::try_parse_from(["forja", "serve", "--port", "7777"]).expect("parsear serve");
        match cli.command {
            Command::Serve {
                port,
                auth_token,
                open,
            } => {
                assert_eq!(port, 7777);
                assert!(auth_token.is_none());
                assert!(!open);
            }
            other => panic!("subcomando inesperado: {other:?}"),
        }
    }

    #[test]
    fn parse_run_accepts_opencode_hermes_and_local() {
        for (raw, expected) in [
            ("claude", CliAgent::Claude),
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
        for name in ["mock", "agy", "claude", "opencode", "hermes", "local"] {
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
