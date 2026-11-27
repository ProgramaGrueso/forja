use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

pub mod app;
mod friendly;
pub mod init;
pub mod skills;

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

        /// Instala también el lanzador de escritorio (`forja.desktop` + icono).
        #[arg(long)]
        desktop: bool,
    },

    /// Abre Forja como app: arranca el panel en segundo plano (si no está ya) y lo
    /// abre en el navegador. Config en `app.json`, estado y log en el directorio de estado.
    App {
        /// Detiene el Forja lanzado por `forja app`.
        #[arg(long, conflicts_with = "status")]
        stop: bool,

        /// Dice si está en marcha.
        #[arg(long)]
        status: bool,
    },

    /// Lanzador de escritorio (`~/.local/share/applications/forja.desktop`).
    Desktop {
        #[command(subcommand)]
        action: DesktopAction,
    },
    /// Skills genéricas incluidas en Forja: listar, ver estado, instalar y quitar.
    Skills {
        #[command(subcommand)]
        action: SkillsCommand,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum DesktopAction {
    /// Escribe el `.desktop` y copia el icono.
    Install,
    /// Quita el `.desktop` y el icono.
    Uninstall,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SkillsCommand {
    /// Lista las skills incluidas en este binario.
    List,

    /// Muestra, por cada agente instalado, qué skills de Forja ya están.
    Status {
        /// Un agente concreto: claude, opencode, agy o hermes.
        #[arg(long)]
        agent: Option<String>,
        /// Perfil de Hermes (por defecto `default`).
        #[arg(long)]
        profile: Option<String>,
    },

    /// Copia las skills a la carpeta de cada agente. Sin `--yes` solo muestra qué haría.
    Install {
        #[arg(long)]
        agent: Option<String>,
        /// Una skill concreta (repetible). Por defecto, todas.
        #[arg(long)]
        skill: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
        /// Permite sobrescribir una skill existente (nunca sigue enlaces simbólicos).
        #[arg(long)]
        force: bool,
        /// Aplica los cambios (sin esto es un dry-run).
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Quita solo lo que Forja instaló (carpetas con marcador `.forja-skill`).
    Uninstall {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        skill: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
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
        // Las rutas relativas son del repo, no del cwd: `serve --repo X` desde
        // otra carpeta debe usar X/.forja/... (M5).
        cli.store_dir = resolve_data_dir(&cli.store_dir, cli.repo.as_deref());
        cli.worktrees_dir = resolve_data_dir(&cli.worktrees_dir, cli.repo.as_deref());
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
            let (agent_models, agent_effort, sensitive_agents) = load_run_settings(&cli.store_dir);
            let built = agent.build_agent(
                model.as_deref(),
                &agent_models,
                effort.as_deref(),
                &agent_effort,
            )?;
            let scheduler = build_scheduler(
                &cli.store_dir,
                &cli.worktrees_dir,
                cli.repo,
                base_branch,
                sensitive_agents,
            )?;
            let titulo = title.unwrap_or_else(|| derive_title(&prompt));
            let task = Task::new(titulo, prompt, privacy.into());

            let submitted = scheduler.submit(task, built).await?;
            // El agente corre en su propio grupo de procesos y no recibe el Ctrl+C
            // de la terminal: al recibir la señal se cancela y se mata su grupo.
            let finished = tokio::select! {
                finished = scheduler.wait_run(&submitted.id) => finished?,
                sig = forja_server::shutdown_signal() => {
                    scheduler
                        .shutdown(&format!("forja run interrumpido ({sig})"))
                        .await;
                    scheduler
                        .get_run(&submitted.id)
                        .await
                        .ok_or_else(|| ForjaError::NotFound(submitted.id.clone()))?
                }
            };

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
                    &init::InitEnv::detect(&cli.store_dir),
                )?;
            }
            Ok(())
        }
        Command::Init { yes, desktop } => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            init::run_init(
                &mut input,
                out,
                &cli.store_dir,
                yes,
                &init::InitEnv::detect(&cli.store_dir),
            )?;
            app::init_app_steps(
                &mut input,
                out,
                &app::AppEnv::detect()?,
                yes,
                desktop,
                friendly::detect_repo(),
            )
        }
        Command::App { stop, status } => {
            let env = app::AppEnv::detect()?;
            if stop {
                app::app_stop(&env, out)
            } else if status {
                app::app_status(&env, out).map(|_| ())
            } else {
                app::app_start(&env, out, cli.repo.clone().or_else(friendly::detect_repo))
            }
        }
        Command::Desktop { action } => {
            let env = app::AppEnv::detect()?;
            match action {
                DesktopAction::Install => app::desktop_install(&env, out),
                DesktopAction::Uninstall => app::desktop_uninstall(&env, out),
            }
        }
        Command::Skills { action } => {
            let selection = |agent: Option<String>, skill: Vec<String>, profile: Option<String>| {
                skills::Selection {
                    agents: agent.into_iter().collect(),
                    skills: skill,
                    profile,
                }
            };
            match action {
                SkillsCommand::List => skills::cmd_list(out),
                SkillsCommand::Status { agent, profile } => {
                    let env = skills::SkillsEnv::detect(&cli.store_dir)?;
                    skills::cmd_status(out, &env, &selection(agent, Vec::new(), profile))
                }
                SkillsCommand::Install {
                    agent,
                    skill,
                    profile,
                    force,
                    yes,
                } => {
                    let env = skills::SkillsEnv::detect(&cli.store_dir)?;
                    skills::cmd_install(out, &env, &selection(agent, skill, profile), force, yes)
                }
                SkillsCommand::Uninstall {
                    agent,
                    skill,
                    profile,
                    yes,
                } => {
                    let env = skills::SkillsEnv::detect(&cli.store_dir)?;
                    skills::cmd_uninstall(out, &env, &selection(agent, skill, profile), yes)
                }
            }
        }
        Command::Serve {
            port,
            auth_token,
            open,
        } => {
            let settings_path = cli.store_dir.join("settings.json");
            let (persisted, settings_warning) =
                forja_server::settings::load_settings_or_warn(&settings_path);
            if let Some(warning) = &settings_warning {
                eprintln!("aviso: {warning}");
            }

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
            let sensitive_agents = persisted
                .as_ref()
                .map(|s| s.sensitive_agents.clone())
                .unwrap_or_else(forja_core::default_sensitive_agents);

            let store = JsonlStore::new(&cli.store_dir)?;
            let mut sched_cfg = SchedulerConfig::new(&cli.worktrees_dir);
            sched_cfg.max_concurrency = max_concurrency;
            let sched_repo = cli.repo.clone();
            sched_cfg.repo_path = cli.repo;
            sched_cfg.base_branch = base_branch.clone();
            sched_cfg.sensitive_agents = sensitive_agents.clone();
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
                sensitive_agents,
            };

            let mut state = AppState::new(scheduler)
                .with_settings(settings)
                .with_settings_path(settings_path)
                .with_settings_warning(settings_warning);
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

/// Modelos, esfuerzos y política `sensitive` de `settings.json` (defaults estrictos si no existe o es inválido).
fn load_run_settings(
    store_dir: &std::path::Path,
) -> (
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    Vec<AgentKind>,
) {
    let (settings, warning) =
        forja_server::settings::load_settings_or_warn(&store_dir.join("settings.json"));
    if let Some(warning) = warning {
        eprintln!("aviso: {warning}");
    }
    settings
        .map(|s| (s.agent_models, s.agent_effort, s.sensitive_agents))
        .unwrap_or_else(|| {
            (
                BTreeMap::new(),
                BTreeMap::new(),
                forja_core::default_sensitive_agents(),
            )
        })
}

/// Resuelve una carpeta de datos relativa contra el repo (o el cwd sin repo).
///
/// Compatibilidad: si la ruta relativa ya existe en el cwd y no en el repo
/// (datos creados antes de este cambio), se sigue usando la del cwd con un aviso;
/// nunca se mueven datos.
fn resolve_data_dir(dir: &std::path::Path, repo: Option<&std::path::Path>) -> PathBuf {
    if dir.is_absolute() {
        return dir.to_path_buf();
    }
    let from_cwd = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let Some(repo) = repo else {
        return from_cwd;
    };
    let repo = std::path::absolute(repo).unwrap_or_else(|_| repo.to_path_buf());
    let from_repo = repo.join(dir);
    let same = from_cwd == from_repo
        || matches!(
            (from_cwd.canonicalize(), from_repo.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        );
    if !same && from_cwd.exists() && !from_repo.exists() {
        eprintln!(
            "forja: aviso: uso {} (datos existentes en la carpeta actual) en lugar de {}",
            from_cwd.display(),
            from_repo.display()
        );
        return from_cwd;
    }
    from_repo
}

fn build_scheduler(
    store_dir: &PathBuf,
    worktrees_dir: &PathBuf,
    repo: Option<PathBuf>,
    base_branch: String,
    sensitive_agents: Vec<AgentKind>,
) -> Result<Scheduler> {
    let store = JsonlStore::new(store_dir)?;
    let mut config = SchedulerConfig::new(worktrees_dir);
    config.repo_path = repo;
    config.base_branch = base_branch;
    config.sensitive_agents = sensitive_agents;
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
        assert!(matches!(cli.command, Command::Init { yes: true, .. }));
        let cli = Cli::try_parse_from(["forja", "init"]).expect("init");
        assert!(matches!(cli.command, Command::Init { yes: false, .. }));
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

    #[tokio::test]
    async fn run_sensitive_applies_default_without_settings_and_respects_settings_file() {
        let store_dir = TempDir::new().expect("crear store tempdir");
        let wt_dir = TempDir::new().expect("crear worktrees tempdir");

        // Sin settings.json, el default estricto (solo claude) rechaza mock con sensitive.
        let cli_default = Cli::try_parse_from([
            "forja",
            "--store-dir",
            store_dir.path().to_str().expect("store utf8"),
            "--worktrees-dir",
            wt_dir.path().to_str().expect("wt utf8"),
            "run",
            "--agent",
            "mock",
            "--privacy",
            "sensitive",
            "secreto",
        ])
        .expect("parsear run");
        let mut buf = Vec::new();
        let err = execute_with_writer(cli_default, &mut buf)
            .await
            .expect_err("sin settings.json, mock + sensitive debe rechazarse");
        assert!(matches!(
            err,
            ForjaError::PolicyDenied {
                privacy: Privacy::Sensitive,
                agent: AgentKind::Mock
            }
        ));

        // Con settings.json que permite ["mock"], mock pasa y agy sigue rechazado.
        let mut settings = forja_server::settings::Settings::with_defaults("/tmp/forja-test-store");
        settings.sensitive_agents = vec![AgentKind::Mock];
        settings.validate().expect("ajustes de prueba válidos");
        forja_server::settings::save_settings(&store_dir.path().join("settings.json"), &settings)
            .expect("guardar settings");

        let cli_allowed = Cli::try_parse_from([
            "forja",
            "--store-dir",
            store_dir.path().to_str().expect("store utf8"),
            "--worktrees-dir",
            wt_dir.path().to_str().expect("wt utf8"),
            "run",
            "--agent",
            "mock",
            "--privacy",
            "sensitive",
            "secreto permitido",
        ])
        .expect("parsear run");
        let mut ok_buf = Vec::new();
        execute_with_writer(cli_allowed, &mut ok_buf)
            .await
            .expect("mock + sensitive debe permitirse con settings.json");
        let out = String::from_utf8(ok_buf).expect("utf8");
        assert!(out.contains("estado: done"), "{out}");

        let cli_denied = Cli::try_parse_from([
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
            "secreto denegado",
        ])
        .expect("parsear run");
        let mut denied_buf = Vec::new();
        let err = execute_with_writer(cli_denied, &mut denied_buf)
            .await
            .expect_err("agy + sensitive debe seguir denegado");
        assert!(matches!(
            err,
            ForjaError::PolicyDenied {
                privacy: Privacy::Sensitive,
                agent: AgentKind::Agy
            }
        ));
    }

    #[test]
    fn relative_data_dirs_resolve_against_the_repo_not_the_cwd() {
        // M5: `serve --repo X` desde otra carpeta usa X/.forja/...
        let repo = TempDir::new().expect("repo");
        let rel = std::path::Path::new(".forja-prueba-m5/store");
        let resolved = crate::resolve_data_dir(rel, Some(repo.path()));
        assert_eq!(resolved, repo.path().join(rel));
        // Absolutas se respetan; sin repo, se resuelven contra el cwd.
        let abs = repo.path().join("otra");
        assert_eq!(crate::resolve_data_dir(&abs, Some(repo.path())), abs);
        let no_repo = crate::resolve_data_dir(rel, None);
        assert!(no_repo.is_absolute() && no_repo.ends_with(rel));
    }

    #[test]
    fn legacy_data_in_cwd_is_kept_without_moving_it() {
        let repo = TempDir::new().expect("repo");
        let cwd = std::env::current_dir().expect("cwd");
        let rel = std::path::Path::new("target/forja-m5-legado-prueba");
        std::fs::create_dir_all(cwd.join(rel)).expect("crear legado");
        let resolved = crate::resolve_data_dir(rel, Some(repo.path()));
        let _ = std::fs::remove_dir(cwd.join(rel));
        assert_eq!(resolved, cwd.join(rel));
    }
}
