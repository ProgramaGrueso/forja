use forja_core::{AgentKind, ForjaError};
use serde_json::Value;
use std::sync::Arc;

use crate::{Agent, AgentFuture, AgentOutput, RunContext, Runner};

/// Modelo por defecto del adaptador OpenCode.
pub const DEFAULT_MODEL: &str = "opencode-go/deepseek-v4.1-flash";

/// Título con el que se identifican las sesiones creadas por Forja.
pub const DEFAULT_TITLE: &str = "forja";

/// Nombre del binario de OpenCode en el `PATH`.
pub const DEFAULT_PROGRAM: &str = "opencode";

/// Argumentos con los que se invoca `opencode run` (sin incluir el programa).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeCommandOptions {
    pub prompt: String,
    pub model: String,
    pub title: String,
    /// Variante de razonamiento: se añade como `#variante` al modelo (reemplaza la
    /// que ya traiga). None = el modelo va tal cual.
    pub effort: Option<String>,
}

impl OpencodeCommandOptions {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: DEFAULT_MODEL.to_string(),
            title: DEFAULT_TITLE.to_string(),
            effort: None,
        }
    }
}

/// Construye el vector de argumentos de `opencode run` sin ejecutar nada.
///
/// El prompt viaja como argumento posicional íntegro (comillas y saltos de
/// línea incluidos); nunca se interpreta a través de una shell.
pub fn build_command(opts: &OpencodeCommandOptions) -> Vec<String> {
    vec![
        DEFAULT_PROGRAM.to_string(),
        "run".to_string(),
        "--model".to_string(),
        crate::models::opencode_model_with_effort(&opts.model, opts.effort.as_deref()),
        "--auto".to_string(),
        "--title".to_string(),
        opts.title.clone(),
        opts.prompt.clone(),
    ]
}

const TEXT_FIELDS: [&str; 5] = ["text", "response", "result", "output", "message"];

fn extract_text_from_value(value: &Value) -> Option<String> {
    let obj = value.as_object()?;
    for field in TEXT_FIELDS {
        if let Some(text) = obj.get(field).and_then(Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

/// Parsea la salida de OpenCode de forma tolerante.
///
/// Se usa el formato de texto por defecto, así que lo normal es devolver el
/// stdout tal cual. Si OpenCode imprime un objeto JSON con un campo de texto
/// conocido, se extrae; cualquier otra cosa se devuelve en crudo junto a un
/// aviso. Esta función nunca entra en `panic`.
pub fn parse_opencode_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> AgentOutput {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return AgentOutput {
            text: String::new(),
            raw_stdout: stdout.to_string(),
            raw_stderr: stderr.to_string(),
            exit_code,
            warnings: vec!["salida vacía de opencode".to_string()],
        };
    }

    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(text) = extract_text_from_value(&value) {
                return AgentOutput {
                    text,
                    raw_stdout: stdout.to_string(),
                    raw_stderr: stderr.to_string(),
                    exit_code,
                    warnings: Vec::new(),
                };
            }

            return AgentOutput {
                text: trimmed.to_string(),
                raw_stdout: stdout.to_string(),
                raw_stderr: stderr.to_string(),
                exit_code,
                warnings: vec![
                    "el JSON de salida de opencode no contiene campos conocidos (text, response, result, output, message)"
                        .to_string(),
                ],
            };
        }
    }

    AgentOutput {
        text: trimmed.to_string(),
        raw_stdout: stdout.to_string(),
        raw_stderr: stderr.to_string(),
        exit_code,
        warnings: Vec::new(),
    }
}

/// Adaptador real de OpenCode sobre `opencode run`.
#[derive(Debug, Clone)]
pub struct OpencodeAgent {
    pub program: String,
    pub model: String,
    pub title: String,
    /// Variante de razonamiento (`#variante` del modelo); None = no se añade.
    pub effort: Option<String>,
}

impl Default for OpencodeAgent {
    fn default() -> Self {
        Self::new(DEFAULT_MODEL)
    }
}

impl OpencodeAgent {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            program: DEFAULT_PROGRAM.to_string(),
            model: model.into(),
            title: DEFAULT_TITLE.to_string(),
            effort: None,
        }
    }

    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        self.effort = Some(effort.into());
        self
    }

    /// Cambia el binario a ejecutar (los tests apuntan a un script falso).
    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
}

impl Agent for OpencodeAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Opencode
    }

    fn model(&self) -> Option<String> {
        Some(self.model.clone())
    }

    fn with_model(&self, model: &str) -> Option<Arc<dyn Agent>> {
        let mut copy = self.clone();
        copy.model = model.to_string();
        Some(Arc::new(copy))
    }

    fn effort(&self) -> Option<String> {
        self.effort.clone()
    }

    fn with_effort(&self, effort: &str) -> Option<Arc<dyn Agent>> {
        let mut copy = self.clone();
        copy.effort = Some(effort.to_string());
        Some(Arc::new(copy))
    }

    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            let opts = OpencodeCommandOptions {
                prompt: ctx.task.prompt.clone(),
                model: self.model.clone(),
                title: self.title.clone(),
                effort: self.effort.clone(),
            };
            let mut argv = build_command(&opts);
            argv[0] = self.program.clone();

            let mut result = Runner::run_argv(&argv, &ctx.worktree, ctx.timeout_secs).await;
            let mut dropped_variant = false;
            // No todos los modelos tienen variantes: si opencode lo dice, se reintenta
            // UNA vez sin la variante (dentro del adaptador, no cuenta como intento).
            if let Err(ForjaError::Agent(msg)) = &result {
                if msg.contains("Variant unavailable") && argv[3].contains('#') {
                    let note = format!("{}\n", crate::EFFORT_NOT_APPLIED_WARNING);
                    crate::emit_to_sink(crate::OutputStream::Stderr, &note);
                    argv[3] = argv[3].split('#').next().unwrap_or(&argv[3]).to_string();
                    dropped_variant = true;
                    result = Runner::run_argv(&argv, &ctx.worktree, ctx.timeout_secs).await;
                }
            }

            match result {
                Ok(res) => {
                    let mut out = parse_opencode_output(&res.stdout, &res.stderr, res.exit_code);
                    if dropped_variant {
                        out.warnings
                            .push(crate::EFFORT_NOT_APPLIED_WARNING.to_string());
                        out.raw_stderr = format!(
                            "{}\n{}",
                            crate::EFFORT_NOT_APPLIED_WARNING,
                            out.raw_stderr
                        );
                        out.text = format!("({})\n{}", crate::EFFORT_NOT_APPLIED_WARNING, out.text);
                    }
                    Ok(out)
                }
                Err(ForjaError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                    Err(ForjaError::Agent(format!(
                        "no se encontró el programa '{}'; verificá que OpenCode esté instalado y en el PATH",
                        self.program
                    )))
                }
                Err(err) => Err(err),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn opencode_effort_is_a_model_variant_and_replaces_existing() {
        let mut opts = OpencodeCommandOptions::new("p");
        opts.model = "prov/mod".into();
        assert_eq!(build_command(&opts)[3], "prov/mod");
        opts.effort = Some("high".into());
        let args = build_command(&opts);
        assert_eq!(args[3], "prov/mod#high");
        assert!(!args.contains(&"--effort".to_string()));
        opts.model = "prov/mod#low".into();
        assert_eq!(build_command(&opts)[3], "prov/mod#high");
        opts.effort = None;
        assert_eq!(build_command(&opts)[3], "prov/mod#low");
        let agent = Agent::with_effort(&OpencodeAgent::default(), "max").unwrap();
        assert_eq!(agent.effort().as_deref(), Some("max"));
    }

    use std::path::PathBuf;

    use forja_core::{AgentKind, ForjaError, Privacy, Task};
    use tempfile::tempdir;

    use super::*;

    fn fake_opencode_path() -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_opencode.sh")
            .to_string_lossy()
            .into_owned()
    }

    fn ctx_with_prompt(dir: &std::path::Path, prompt: &str, timeout_secs: u64) -> RunContext {
        RunContext {
            task: Task::new("Prueba OpenCode", prompt, Privacy::Open),
            worktree: dir.to_path_buf(),
            timeout_secs,
        }
    }

    #[test]
    fn opencode_agent_keeps_model_with_default() {
        let agent = OpencodeAgent::default();
        assert_eq!(agent.program, DEFAULT_PROGRAM);
        assert_eq!(agent.model, DEFAULT_MODEL);
        assert_eq!(agent.model, "opencode-go/deepseek-v4.1-flash");
        assert_eq!(agent.title, DEFAULT_TITLE);
        assert_eq!(OpencodeAgent::new("otro/modelo").model, "otro/modelo");
    }

    #[test]
    fn opencode_args_basic_order() {
        let opts = OpencodeCommandOptions::new("implementa V02");
        let args = build_command(&opts);

        assert_eq!(
            args,
            vec![
                "opencode",
                "run",
                "--model",
                "opencode-go/deepseek-v4.1-flash",
                "--auto",
                "--title",
                "forja",
                "implementa V02",
            ]
        );
    }

    #[test]
    fn opencode_args_prompt_with_quotes_and_newlines() {
        let prompt = "Di \"hola\" y 'chau'\nlinea 2: $HOME `whoami` ; rm -rf /";
        let opts = OpencodeCommandOptions::new(prompt);
        let args = build_command(&opts);

        assert_eq!(args.len(), 8);
        assert_eq!(args[0], "opencode");
        assert_eq!(args[1], "run");
        assert!(args.contains(&"--auto".to_string()));
        assert_eq!(args[7], prompt);
    }

    #[test]
    fn opencode_args_custom_model_and_title() {
        let mut opts = OpencodeCommandOptions::new("p");
        opts.model = "provider/modelo".to_string();
        opts.title = "mi-sesion".to_string();
        let args = build_command(&opts);

        assert_eq!(args[3], "provider/modelo");
        assert_eq!(args[6], "mi-sesion");
        assert_eq!(args[7], "p");
    }

    #[test]
    fn parse_opencode_plain_text() {
        let out = parse_opencode_output("trabajo terminado\n", "", Some(0));

        assert_eq!(out.text, "trabajo terminado");
        assert_eq!(out.raw_stdout, "trabajo terminado\n");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn parse_opencode_empty_output_warns() {
        let out = parse_opencode_output("   \n\t  ", "", Some(0));

        assert!(out.text.is_empty());
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn parse_opencode_json_with_known_field() {
        let out = parse_opencode_output(r#"{"result":"respuesta json"}"#, "", Some(0));

        assert_eq!(out.text, "respuesta json");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn parse_opencode_broken_json_returns_raw_text() {
        let out = parse_opencode_output("{roto", "aviso", Some(0));

        assert_eq!(out.text, "{roto");
        assert_eq!(out.raw_stderr, "aviso");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn parse_opencode_json_without_known_fields_warns() {
        let raw = r#"{"status":"ok","otro":1}"#;
        let out = parse_opencode_output(raw, "", Some(0));

        assert_eq!(out.text, raw);
        assert!(!out.warnings.is_empty());
    }

    #[tokio::test]
    async fn opencode_agent_runs_fake_script_with_prompt_as_one_arg() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fake_opencode_record"), "").expect("marcador");

        let agent = OpencodeAgent::default().with_program(fake_opencode_path());
        assert_eq!(agent.kind(), AgentKind::Opencode);

        let prompt = "tarea con \"comillas\"\ny salto de línea";
        let ctx = ctx_with_prompt(dir.path(), prompt, 5);

        let out = agent
            .run(&ctx)
            .await
            .expect("fake_opencode debe responder ok");
        assert_eq!(out.text, "trabajo listo desde fake_opencode");
        assert_eq!(out.exit_code, Some(0));
        assert!(out.warnings.is_empty());

        let recorded = std::fs::read_to_string(dir.path().join("fake_opencode_args"))
            .expect("el script debe registrar sus argumentos");
        let args: Vec<&str> = recorded.split('\0').filter(|arg| !arg.is_empty()).collect();
        assert_eq!(args.last().copied(), Some(prompt));
        assert!(args.contains(&"--auto"));
        assert!(args.contains(&"--title") && args.contains(&"forja"));

        let cwd = std::fs::read_to_string(dir.path().join("fake_opencode_cwd")).expect("cwd");
        assert_eq!(
            std::fs::canonicalize(cwd.trim()).expect("cwd canónico"),
            std::fs::canonicalize(dir.path()).expect("dir canónico")
        );
    }

    #[tokio::test]
    async fn opencode_agent_nonzero_exit_is_agent_error() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fake_opencode_exit"), "2\n").expect("exit");
        std::fs::write(dir.path().join("fake_opencode_stderr"), "falló la sesión\n")
            .expect("stderr");

        let agent = OpencodeAgent::default().with_program(fake_opencode_path());
        let ctx = ctx_with_prompt(dir.path(), "p", 5);

        let err = agent
            .run(&ctx)
            .await
            .expect_err("código distinto de 0 debe fallar");
        match err {
            ForjaError::Agent(msg) => {
                assert!(msg.contains('2'), "esperaba el código 2 en: {msg}");
                assert!(msg.contains("falló la sesión"), "esperaba stderr en: {msg}");
            }
            other => panic!("esperaba ForjaError::Agent, obtuve: {other:?}"),
        }
    }

    #[tokio::test]
    async fn opencode_variant_unavailable_retries_once_without_variant() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fake_opencode_record"), "").unwrap();
        std::fs::write(dir.path().join("fake_opencode_no_variants"), "").unwrap();
        let agent = OpencodeAgent::default()
            .with_program(fake_opencode_path())
            .with_effort("high");
        let ctx = ctx_with_prompt(dir.path(), "p", 5);
        let out = agent
            .run(&ctx)
            .await
            .expect("el reintento sin variante debe pasar");
        assert!(out.text.contains("se ejecutó sin variante"), "{}", out.text);
        assert!(out.text.contains("trabajo listo"));
        assert!(out
            .warnings
            .contains(&crate::EFFORT_NOT_APPLIED_WARNING.to_string()));
        let recorded = std::fs::read_to_string(dir.path().join("fake_opencode_args")).unwrap();
        let args: Vec<&str> = recorded.split('\0').filter(|a| !a.is_empty()).collect();
        assert!(
            args.contains(&"opencode-go/deepseek-v4.1-flash"),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a.contains('#')), "{args:?}");
    }

    #[tokio::test]
    async fn opencode_variant_error_without_variant_is_not_retried() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("fake_opencode_stderr"),
            "Error: Variant unavailable for x: y\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("fake_opencode_exit"), "1\n").unwrap();
        let agent = OpencodeAgent::default().with_program(fake_opencode_path());
        let ctx = ctx_with_prompt(dir.path(), "p", 5);
        assert!(agent.run(&ctx).await.is_err());
    }

    #[tokio::test]
    async fn opencode_agent_timeout() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fake_opencode_sleep"), "3\n").expect("sleep");

        let agent = OpencodeAgent::default().with_program(fake_opencode_path());
        let ctx = ctx_with_prompt(dir.path(), "p", 1);

        let err = agent
            .run(&ctx)
            .await
            .expect_err("sleep 3s con timeout 1s debe expirar");
        assert!(matches!(err, ForjaError::Timeout { seconds: 1 }));
    }

    #[tokio::test]
    async fn opencode_agent_empty_output_warns() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fake_opencode_stdout"), "").expect("stdout vacío");

        let agent = OpencodeAgent::default().with_program(fake_opencode_path());
        let ctx = ctx_with_prompt(dir.path(), "p", 5);

        let out = agent
            .run(&ctx)
            .await
            .expect("salida vacía no es error de proceso");
        assert!(out.text.is_empty());
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.warnings.is_empty());
    }

    #[tokio::test]
    async fn opencode_agent_missing_program_reports_clear_error() {
        let dir = tempdir().expect("tempdir");
        let agent = OpencodeAgent::default().with_program("opencode-inexistente-forja-v02");
        let ctx = ctx_with_prompt(dir.path(), "p", 5);

        let err = agent
            .run(&ctx)
            .await
            .expect_err("programa ausente debe fallar");
        match err {
            ForjaError::Agent(msg) => assert!(
                msg.contains("opencode-inexistente-forja-v02"),
                "el mensaje debe nombrar el programa: {msg}"
            ),
            other => panic!("esperaba ForjaError::Agent, obtuve: {other:?}"),
        }
    }
}
