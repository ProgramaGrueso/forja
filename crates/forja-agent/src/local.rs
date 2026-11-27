use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use forja_core::{AgentKind, ForjaError, Privacy, Result};

use crate::runner::{CommandResult, Runner};
use crate::{Agent, AgentFuture, AgentOutput, RunContext};

/// Alias de modelo local por defecto.
pub const DEFAULT_MODEL: &str = "moe";
/// Programa por defecto: el CLI de OpenCode, que habla con `llm` vía su proveedor local.
pub const DEFAULT_PROGRAM: &str = "opencode";
/// Puerto por defecto del servidor OpenAI-compatible de `llm`.
pub const DEFAULT_PORT: u16 = 8081;
/// Alias corto del modelo MoE.
pub const MOE_ALIAS: &str = "moe";
/// Nombre real del modelo MoE.
pub const MOE_MODEL: &str = "qwen3.8-35b-a3b-256k";
/// Alias corto del modelo rápido.
pub const FLASH_ALIAS: &str = "flash";
/// Nombre real del modelo rápido.
pub const FLASH_MODEL: &str = "flashnext-64k";
/// Prefijo del proveedor local dentro de OpenCode.
pub const PROVIDER: &str = "llm-local";
/// Tiempo máximo del sondeo TCP al modelo local.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Resuelve un alias corto a su nombre real de modelo (sin tocar valores ya cualificados).
pub fn resolve_alias(model: &str) -> String {
    match model {
        MOE_ALIAS => MOE_MODEL.to_string(),
        FLASH_ALIAS => FLASH_MODEL.to_string(),
        other => other.to_string(),
    }
}

/// Devuelve el valor que se pasa a `-m`: `llm-local/<modelo>` salvo que ya venga cualificado.
pub fn model_target(model: &str) -> String {
    if model.contains('/') {
        model.to_string()
    } else {
        format!("{PROVIDER}/{}", resolve_alias(model))
    }
}

/// `true` si el modelo se sirve en esta máquina (proveedor `llm-local`).
///
/// `local` pasa el modelo tal cual a `opencode -m`: un valor cualificado como
/// `openrouter/vendor/modelo` saldría a la nube. Solo `llm-local/...` y los
/// alias sin proveedor (que se resuelven a `llm-local/...`) son locales.
pub fn is_local_model(model: &str) -> bool {
    model_target(model.trim()).starts_with(&format!("{PROVIDER}/"))
}

/// Mensaje de rechazo para tareas `sensitive` con un modelo no local.
pub fn non_local_model_message(model: &str) -> String {
    format!(
        "privacidad sensitive: el agente local solo admite modelos locales ({PROVIDER}/... o los alias {MOE_ALIAS}/{FLASH_ALIAS}); \"{model}\" es de un proveedor externo"
    )
}

/// Opciones puras para construir la invocación de `opencode run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCommandOptions {
    pub program: String,
    pub prompt: String,
    pub model: String,
}

impl LocalCommandOptions {
    pub fn new(prompt: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            program: DEFAULT_PROGRAM.to_string(),
            prompt: prompt.into(),
            model: model.into(),
        }
    }
}

/// Construye el vector de argumentos sin ejecutar nada y sin pasar por una shell.
///
/// Forma: `<programa> run -m llm-local/<modelo> --auto <mensaje>`.
pub fn build_command(opts: &LocalCommandOptions) -> Vec<String> {
    vec![
        opts.program.clone(),
        "run".to_string(),
        "-m".to_string(),
        model_target(&opts.model),
        "--auto".to_string(),
        crate::positional_prompt(&opts.prompt),
    ]
}

/// Comprueba con una conexión TCP corta que el servidor local acepta conexiones.
///
/// No arranca ni reinicia nada: solo mira si el puerto responde en `127.0.0.1`.
pub fn port_is_open(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

/// Mensaje exacto cuando el modelo local no está levantado.
pub fn offline_message() -> String {
    "modelo local apagado: ejecuta `llm on moe`".to_string()
}

/// Parseo tolerante: `opencode run` responde texto plano; si viene vacío se avisa sin fallar.
pub fn parse_local_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> AgentOutput {
    let text = stdout.trim();
    let warnings = if text.is_empty() {
        vec!["salida vacía de opencode".to_string()]
    } else {
        Vec::new()
    };

    AgentOutput {
        text: text.to_string(),
        raw_stdout: stdout.to_string(),
        raw_stderr: stderr.to_string(),
        exit_code,
        warnings,
    }
}

/// Adaptador de modelos locales servidos por `llm` (API OpenAI-compatible).
#[derive(Debug, Clone)]
pub struct LocalAgent {
    /// Programa a ejecutar; por defecto `opencode` y configurable para los tests.
    pub program: String,
    /// Alias o modelo cualificado (`moe`, `flash`, `llm-local/...`).
    pub model: String,
    /// Puerto local donde escucha `llm`.
    pub port: u16,
}

impl Default for LocalAgent {
    fn default() -> Self {
        Self::new(DEFAULT_MODEL)
    }
}

impl LocalAgent {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            program: DEFAULT_PROGRAM.to_string(),
            model: model.into(),
            port: DEFAULT_PORT,
        }
    }

    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    fn command_options(&self, ctx: &RunContext) -> LocalCommandOptions {
        LocalCommandOptions {
            program: self.program.clone(),
            prompt: ctx.task.prompt.clone(),
            model: self.model.clone(),
        }
    }

    async fn execute(&self, ctx: &RunContext) -> Result<CommandResult> {
        let argv = build_command(&self.command_options(ctx));
        Runner::run_argv(&argv, &ctx.worktree, ctx.timeout_secs)
            .await
            .map_err(|err| match err {
                ForjaError::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => {
                    ForjaError::Agent(format!(
                        "programa '{}' no encontrado: {io_err}",
                        self.program
                    ))
                }
                other => other,
            })
    }
}

impl Agent for LocalAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Local
    }

    fn model(&self) -> Option<String> {
        Some(self.model.clone())
    }

    fn with_model(&self, model: &str) -> Option<Arc<dyn Agent>> {
        let mut copy = self.clone();
        copy.model = model.to_string();
        Some(Arc::new(copy))
    }

    fn privacy_violation(&self, privacy: Privacy) -> Option<String> {
        (privacy == Privacy::Sensitive && !is_local_model(&self.model))
            .then(|| non_local_model_message(&self.model))
    }

    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            // Defensa en profundidad: aunque se salte la validación al encolar,
            // un prompt sensitive nunca sale hacia un proveedor externo.
            if let Some(reason) = self.privacy_violation(ctx.task.privacidad) {
                return Err(ForjaError::InvalidInput(reason));
            }
            // Regla dura: la GPU tiene 16 GB y no debe colapsar. Si el modelo local
            // no está escuchando, se falla sin arrancar nada ni invocar a `llm`.
            if !port_is_open(self.port) {
                return Err(ForjaError::Agent(offline_message()));
            }

            let res = self.execute(ctx).await?;
            Ok(parse_local_output(&res.stdout, &res.stderr, res.exit_code))
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_does_not_support_effort() {
        assert!(Agent::with_effort(&LocalAgent::default(), "high").is_none());
        assert_eq!(LocalAgent::default().effort(), None);
    }

    use std::net::TcpListener;
    use std::path::{Path, PathBuf};

    use forja_core::{AgentKind, ForjaError, Privacy, Task};
    use tempfile::tempdir;

    use super::*;

    fn fake(name: &str) -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    /// Reserva un puerto efímero y deja un listener vivo para que el sondeo TCP funcione.
    fn live_port() -> (TcpListener, u16) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener efímero");
        let port = listener.local_addr().expect("addr").port();
        (listener, port)
    }

    /// Puerto en el que nada escucha. Se usa el 1 (tcpmux) y no uno efímero liberado:
    /// otro test en paralelo podría ocuparlo justo antes del sondeo.
    fn free_port() -> u16 {
        1
    }

    fn ctx(worktree: &Path, prompt: &str, timeout_secs: u64) -> RunContext {
        RunContext {
            task: Task::new("Prueba local", prompt, Privacy::Open),
            worktree: worktree.to_path_buf(),
            timeout_secs,
        }
    }

    #[test]
    fn local_args_default_model_and_order() {
        let opts = LocalCommandOptions::new("implementa V04", "moe");
        let args = build_command(&opts);

        assert_eq!(
            args,
            vec![
                "opencode",
                "run",
                "-m",
                "llm-local/qwen3.8-35b-a3b-256k",
                "--auto",
                "implementa V04",
            ]
        );
    }

    #[test]
    fn local_args_prompt_with_quotes_and_newlines() {
        let prompt = "Di \"hola\" y 'adiós'\nsegunda línea: $HOME `whoami` ; rm -rf /";
        let args = build_command(&LocalCommandOptions::new(prompt, "moe"));

        assert_eq!(args.len(), 6);
        assert_eq!(args[1], "run");
        assert_eq!(args[5], prompt);
        assert!(
            args[5].contains('\n'),
            "el prompt debe conservar el salto de línea"
        );
    }

    #[test]
    fn local_args_model_aliases_and_qualified_values() {
        assert_eq!(resolve_alias("moe"), MOE_MODEL);
        assert_eq!(resolve_alias("flash"), FLASH_MODEL);
        assert_eq!(resolve_alias("otro"), "otro");

        let flash = build_command(&LocalCommandOptions::new("p", "flash"));
        assert_eq!(flash[3], "llm-local/flashnext-64k");

        let qualified = build_command(&LocalCommandOptions::new("p", "llm-local/otro#v2"));
        assert_eq!(qualified[3], "llm-local/otro#v2");
    }

    #[test]
    fn local_default_agent_fields() {
        let agent = LocalAgent::default();
        assert_eq!(agent.kind(), AgentKind::Local);
        assert_eq!(agent.model, "moe");
        assert_eq!(agent.program, "opencode");
        assert_eq!(agent.port, 8081);
        assert_eq!(LocalAgent::new("flash").model, "flash");
        assert_eq!(LocalAgent::default().with_port(9000).port, 9000);
    }

    #[test]
    fn local_parse_empty_output_warns() {
        let out = parse_local_output("  \n\t ", "", Some(0));
        assert!(out.text.is_empty());
        assert_eq!(out.raw_stdout, "  \n\t ");
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.warnings.is_empty());

        let plain = parse_local_output("respuesta\n", "", Some(0));
        assert_eq!(plain.text, "respuesta");
        assert!(plain.warnings.is_empty());
    }

    #[tokio::test]
    async fn local_agent_success_with_fake_program() {
        let (_listener, port) = live_port();
        let dir = tempdir().expect("tempdir");
        let agent = LocalAgent::default()
            .with_program(fake("fake_local.sh"))
            .with_port(port);

        let out = agent
            .run(&ctx(dir.path(), "hola", 5))
            .await
            .expect("el script falso debe responder ok");

        assert_eq!(out.text, "ok desde fake_local");
        assert_eq!(out.exit_code, Some(0));
        assert!(out.warnings.is_empty());
    }

    #[tokio::test]
    async fn local_agent_passes_model_and_prompt_as_argv() {
        let (_listener, port) = live_port();
        let dir = tempdir().expect("tempdir");
        let agent = LocalAgent::new("flash")
            .with_program(fake("fake_local_echo_args.sh"))
            .with_port(port);

        let out = agent
            .run(&ctx(dir.path(), "hola mundo", 5))
            .await
            .expect("el script falso debe responder ok");

        let lines: Vec<&str> = out.text.lines().collect();
        assert_eq!(lines.first().copied(), Some("run"));
        assert!(lines.contains(&"-m"));
        assert!(lines.contains(&"llm-local/flashnext-64k"));
        assert!(lines.contains(&"--auto"));
        assert_eq!(lines.last().copied(), Some("hola mundo"));
    }

    #[test]
    fn only_llm_local_models_count_as_local() {
        for local in ["moe", "flash", "qwen3", "llm-local/qwen3", " llm-local/x "] {
            assert!(is_local_model(local), "{local}");
        }
        for cloud in [
            "openrouter/vendor/modelo-nube",
            "anthropic/claude",
            "llm-local-fake/x",
            "x/llm-local/y",
        ] {
            assert!(!is_local_model(cloud), "{cloud}");
        }
        let agent = LocalAgent::new("openrouter/vendor/modelo-nube");
        assert!(agent.privacy_violation(Privacy::Sensitive).is_some());
        assert!(agent.privacy_violation(Privacy::Open).is_none());
        assert!(LocalAgent::default()
            .privacy_violation(Privacy::Sensitive)
            .is_none());
    }

    #[tokio::test]
    async fn sensitive_task_never_reaches_a_cloud_model() {
        let (_listener, port) = live_port();
        let dir = tempdir().expect("tempdir");
        let agent = LocalAgent::new("openrouter/vendor/modelo-nube")
            .with_program(fake("fake_local_echo_args.sh"))
            .with_port(port);
        let mut context = ctx(dir.path(), "datos privados", 5);
        context.task.privacidad = Privacy::Sensitive;
        let err = agent.run(&context).await.expect_err("debe rechazarse");
        assert!(err.to_string().contains("proveedor externo"), "{err}");
    }

    #[tokio::test]
    async fn prompt_starting_with_dash_is_not_an_option() {
        let (_listener, port) = live_port();
        let dir = tempdir().expect("tempdir");
        let agent = LocalAgent::new("flash")
            .with_program(fake("fake_local_echo_args.sh"))
            .with_port(port);
        let out = agent
            .run(&ctx(dir.path(), "--version", 5))
            .await
            .expect("respuesta del script falso");
        assert_eq!(out.text.lines().last(), Some(" --version"));
        assert_eq!(
            build_command(&LocalCommandOptions::new("-h", "moe"))[5],
            " -h"
        );
    }

    #[tokio::test]
    async fn local_agent_nonzero_exit_is_agent_error() {
        let (_listener, port) = live_port();
        let dir = tempdir().expect("tempdir");
        let agent = LocalAgent::default()
            .with_program(fake("fake_local_fail.sh"))
            .with_port(port);

        let err = agent
            .run(&ctx(dir.path(), "hola", 5))
            .await
            .expect_err("código 7 debe fallar");

        match err {
            ForjaError::Agent(msg) => {
                assert!(msg.contains('7'), "esperaba el código 7 en: {msg}");
                assert!(
                    msg.contains("fallo simulado local"),
                    "esperaba el stderr en: {msg}"
                );
            }
            other => panic!("esperaba ForjaError::Agent, obtuve: {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_agent_timeout() {
        let (_listener, port) = live_port();
        let dir = tempdir().expect("tempdir");
        let agent = LocalAgent::default()
            .with_program(fake("fake_local_slow.sh"))
            .with_port(port);

        let err = agent
            .run(&ctx(dir.path(), "hola", 1))
            .await
            .expect_err("sleep 3s con timeout 1s debe expirar");

        assert!(matches!(err, ForjaError::Timeout { seconds: 1 }));
    }

    #[tokio::test]
    async fn local_agent_empty_stdout_returns_warning() {
        let (_listener, port) = live_port();
        let dir = tempdir().expect("tempdir");
        let agent = LocalAgent::default()
            .with_program(fake("fake_local_empty.sh"))
            .with_port(port);

        let out = agent
            .run(&ctx(dir.path(), "hola", 5))
            .await
            .expect("salida vacía no debe fallar");

        assert!(out.text.is_empty());
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.warnings.is_empty());
    }

    #[tokio::test]
    async fn local_agent_missing_program_is_clear_error() {
        let (_listener, port) = live_port();
        let dir = tempdir().expect("tempdir");
        let agent = LocalAgent::default()
            .with_program("forja-programa-inexistente-v04")
            .with_port(port);

        let err = agent
            .run(&ctx(dir.path(), "hola", 5))
            .await
            .expect_err("un programa inexistente debe fallar");

        match err {
            ForjaError::Agent(msg) => {
                assert!(msg.contains("no encontrado"), "mensaje inesperado: {msg}");
                assert!(
                    msg.contains("forja-programa-inexistente-v04"),
                    "mensaje inesperado: {msg}"
                );
            }
            other => panic!("esperaba ForjaError::Agent, obtuve: {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_agent_offline_model_fails_without_running_anything() {
        let port = free_port();
        let dir = tempdir().expect("tempdir");
        // El programa no existe: si se intentara ejecutar, el error sería otro.
        let agent = LocalAgent::default()
            .with_program("forja-programa-inexistente-v04")
            .with_port(port);

        let err = agent
            .run(&ctx(dir.path(), "hola", 5))
            .await
            .expect_err("sin listener en el puerto debe fallar antes de ejecutar");

        assert_eq!(
            err.to_string(),
            "error de agente: modelo local apagado: ejecuta `llm on moe`"
        );
    }

    #[test]
    fn local_port_probe_detects_listener() {
        let (_listener, port) = live_port();
        assert!(port_is_open(port));

        let closed = free_port();
        assert!(!port_is_open(closed));
    }
}
