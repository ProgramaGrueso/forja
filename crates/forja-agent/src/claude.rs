use std::path::Path;

use forja_core::{AgentKind, ForjaError};
use serde_json::Value;

use crate::{Agent, AgentFuture, AgentOutput, RunContext, Runner};

/// Nombre del binario real de Claude Code.
pub const DEFAULT_PROGRAM: &str = "claude";
/// Alias de modelo por defecto; `claude --help` acepta alias como "sonnet".
pub const DEFAULT_MODEL: &str = "sonnet";
/// Modo de permisos por defecto: acepta ediciones de archivos dentro del worktree
/// sin llegar a saltarse todos los permisos (`bypassPermissions`).
pub const DEFAULT_PERMISSION_MODE: &str = "acceptEdits";

/// Opciones puras para construir la línea de argumentos de Claude Code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCommandOptions {
    pub prompt: String,
    pub model: String,
    pub permission_mode: String,
}

impl ClaudeCommandOptions {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: DEFAULT_MODEL.to_string(),
            permission_mode: DEFAULT_PERMISSION_MODE.to_string(),
        }
    }
}

/// Construye `claude -p <prompt> --model <modelo> --output-format json
/// --permission-mode <modo>` sin ejecutar nada. El prompt viaja íntegro como
/// argumento: nunca se interpola en una shell.
pub fn build_command(opts: &ClaudeCommandOptions) -> Vec<String> {
    vec![
        DEFAULT_PROGRAM.to_string(),
        "-p".to_string(),
        opts.prompt.clone(),
        "--model".to_string(),
        opts.model.clone(),
        "--output-format".to_string(),
        "json".to_string(),
        "--permission-mode".to_string(),
        opts.permission_mode.clone(),
    ]
}

const TEXT_FIELDS: [&str; 4] = ["result", "text", "output", "message"];

fn extract_text(value: &Value) -> Option<String> {
    let obj = value.as_object()?;
    for field in TEXT_FIELDS {
        if let Some(text) = obj.get(field).and_then(Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

/// Parsea la salida JSON de `claude --output-format json`. Si no se entiende,
/// devuelve el texto crudo y un aviso; nunca hace panic.
pub fn parse_claude_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> AgentOutput {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return AgentOutput {
            text: String::new(),
            raw_stdout: stdout.to_string(),
            raw_stderr: stderr.to_string(),
            exit_code,
            warnings: vec!["salida vacía de claude".to_string()],
        };
    }

    let parsed = match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => Some((value, trimmed)),
        Err(_) => trimmed
            .lines()
            .rev()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .find_map(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .filter(Value::is_object)
                    .map(|value| (value, line))
            }),
    };

    if let Some((value, raw_json)) = parsed {
        let mut warnings = Vec::new();
        if value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let subtype = value
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("desconocido");
            warnings.push(format!(
                "claude informó is_error=true (subtype: {subtype}); revisa stderr"
            ));
        }

        if let Some(text) = extract_text(&value) {
            return AgentOutput {
                text,
                raw_stdout: stdout.to_string(),
                raw_stderr: stderr.to_string(),
                exit_code,
                warnings,
            };
        }

        warnings.push(
            "el JSON de salida de claude no contiene campos conocidos (result, text, output, message)"
                .to_string(),
        );
        return AgentOutput {
            text: raw_json.to_string(),
            raw_stdout: stdout.to_string(),
            raw_stderr: stderr.to_string(),
            exit_code,
            warnings,
        };
    }

    AgentOutput {
        text: trimmed.to_string(),
        raw_stdout: stdout.to_string(),
        raw_stderr: stderr.to_string(),
        exit_code,
        warnings: vec![
            "la salida de claude no es JSON válido; se devuelve texto crudo".to_string(),
        ],
    }
}

/// Indica si el programa se puede ejecutar: acepta una ruta explícita o busca en
/// `PATH`. No lanza ningún proceso.
pub fn program_available(program: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    match std::env::var_os("PATH") {
        Some(paths) => std::env::split_paths(&paths)
            .filter(|dir| !dir.as_os_str().is_empty())
            .any(|dir| dir.join(program).is_file()),
        None => false,
    }
}

/// Adaptador real de Claude Code (`claude -p`, salida JSON).
#[derive(Debug, Clone)]
pub struct ClaudeAgent {
    /// Programa a ejecutar; por defecto `claude`, configurable para los tests.
    pub program: String,
    pub model: String,
    pub permission_mode: String,
}

impl Default for ClaudeAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeAgent {
    pub fn new() -> Self {
        Self {
            program: DEFAULT_PROGRAM.to_string(),
            model: DEFAULT_MODEL.to_string(),
            permission_mode: DEFAULT_PERMISSION_MODE.to_string(),
        }
    }

    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = mode.into();
        self
    }
}

impl Agent for ClaudeAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            if !program_available(&self.program) {
                return Err(ForjaError::NotConfigured(format!(
                    "binario '{}' no encontrado en PATH; instala Claude Code o ajusta el campo `program` de ClaudeAgent",
                    self.program
                )));
            }

            let opts = ClaudeCommandOptions {
                prompt: ctx.task.prompt.clone(),
                model: self.model.clone(),
                permission_mode: self.permission_mode.clone(),
            };
            let mut argv = build_command(&opts);
            argv[0] = self.program.clone();

            let res = Runner::run_argv(&argv, &ctx.worktree, ctx.timeout_secs).await?;
            Ok(parse_claude_output(&res.stdout, &res.stderr, res.exit_code))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use forja_core::{Privacy, Task};
    use tempfile::tempdir;

    use super::*;

    fn fake_claude_path() -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_claude.sh")
            .to_string_lossy()
            .into_owned()
    }

    fn run_context(dir: &Path, prompt: &str, timeout_secs: u64) -> RunContext {
        RunContext {
            task: Task::new("Prueba claude", prompt, Privacy::Sensitive),
            worktree: dir.to_path_buf(),
            timeout_secs,
        }
    }

    fn set_mode(dir: &Path, mode: &str) {
        fs::write(dir.join("fake_claude_mode.txt"), mode).expect("escribir modo del fake");
    }

    #[test]
    fn claude_args_prompt_with_quotes_and_newlines() {
        let prompt = "Arregla \"esto\" y 'aquello'\nsegunda línea: $HOME `whoami`\n; rm -rf /";
        let opts = ClaudeCommandOptions::new(prompt);
        let args = build_command(&opts);

        assert_eq!(
            args,
            vec![
                "claude",
                "-p",
                prompt,
                "--model",
                "sonnet",
                "--output-format",
                "json",
                "--permission-mode",
                "acceptEdits",
            ]
        );
        assert_eq!(args[2], prompt);
    }

    #[test]
    fn claude_args_defaults_avoid_bypass_permissions() {
        let opts = ClaudeCommandOptions::new("hola");
        let args = build_command(&opts);

        assert_eq!(args[1], "-p");
        assert!(args.iter().any(|a| a == "--output-format"));
        assert!(args.iter().any(|a| a == "json"));
        assert_eq!(args.iter().position(|a| a == "--permission-mode"), Some(7));
        assert_eq!(args[8], "acceptEdits");
        assert!(!args.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(!args.iter().any(|a| a == "bypassPermissions"));
    }

    #[test]
    fn parse_claude_valid_json_result() {
        let stdout =
            r#"{"type":"result","subtype":"success","is_error":false,"result":"tarea completada"}"#;
        let out = parse_claude_output(stdout, "", Some(0));

        assert_eq!(out.text, "tarea completada");
        assert_eq!(out.raw_stdout, stdout);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn parse_claude_broken_json_returns_raw_text_and_warning() {
        let out = parse_claude_output("{roto", "aviso", Some(0));

        assert_eq!(out.text, "{roto");
        assert_eq!(out.raw_stderr, "aviso");
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn parse_claude_empty_output_returns_warning() {
        let out = parse_claude_output("  \n\t ", "", Some(0));

        assert!(out.text.is_empty());
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn parse_claude_error_flag_adds_warning() {
        let stdout = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"me quedé sin turnos"}"#;
        let out = parse_claude_output(stdout, "", Some(0));

        assert_eq!(out.text, "me quedé sin turnos");
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("is_error=true"));
    }

    #[test]
    fn parse_claude_json_without_known_fields_returns_raw_json() {
        let stdout = r#"{"type":"system","session_id":"abc"}"#;
        let out = parse_claude_output(stdout, "", Some(0));

        assert_eq!(out.text, stdout);
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn claude_program_available_for_fake_script_and_missing_path() {
        assert!(program_available(&fake_claude_path()));
        assert!(!program_available("/ruta/que/no/existe/claude-falso"));
        assert!(!program_available("binario-que-no-existe-claude-test"));
    }

    #[tokio::test]
    async fn claude_agent_runs_fake_claude_success() {
        let dir = tempdir().expect("tempdir");
        let agent = ClaudeAgent::new().with_program(fake_claude_path());
        assert_eq!(agent.kind(), AgentKind::Claude);

        let out = agent
            .run(&run_context(dir.path(), "hola", 5))
            .await
            .expect("fake_claude debe responder ok");

        assert_eq!(out.text, "ok desde fake_claude");
        assert_eq!(out.exit_code, Some(0));
        assert!(out.warnings.is_empty());
        assert!(out.raw_stdout.contains("\"result\""));
    }

    #[tokio::test]
    async fn claude_agent_nonzero_exit_returns_agent_error() {
        let dir = tempdir().expect("tempdir");
        set_mode(dir.path(), "exit1");
        let agent = ClaudeAgent::new().with_program(fake_claude_path());

        let err = agent
            .run(&run_context(dir.path(), "rompe", 5))
            .await
            .expect_err("código 1 debe devolver error");

        match err {
            ForjaError::Agent(msg) => {
                assert!(msg.contains('1'), "esperaba el código en: {msg}");
                assert!(
                    msg.contains("fallo simulado de claude"),
                    "esperaba stderr en: {msg}"
                );
            }
            other => panic!("esperaba ForjaError::Agent, obtuve: {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_agent_timeout_returns_timeout_error() {
        let dir = tempdir().expect("tempdir");
        set_mode(dir.path(), "sleep");
        let agent = ClaudeAgent::new().with_program(fake_claude_path());

        let err = agent
            .run(&run_context(dir.path(), "tarda", 1))
            .await
            .expect_err("timeout de 1s debe expirar");

        assert!(matches!(err, ForjaError::Timeout { seconds: 1 }));
    }

    #[tokio::test]
    async fn claude_agent_empty_output_returns_warning() {
        let dir = tempdir().expect("tempdir");
        set_mode(dir.path(), "empty");
        let agent = ClaudeAgent::new().with_program(fake_claude_path());

        let out = agent
            .run(&run_context(dir.path(), "nada", 5))
            .await
            .expect("salida vacía no es un error de proceso");

        assert!(out.text.is_empty());
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.warnings.is_empty());
    }

    #[tokio::test]
    async fn claude_agent_missing_program_returns_not_configured() {
        let dir = tempdir().expect("tempdir");
        let agent = ClaudeAgent::new().with_program("/ruta/que/no/existe/claude-falso");

        let err = agent
            .run(&run_context(dir.path(), "hola", 5))
            .await
            .expect_err("programa ausente debe ser NotConfigured");

        match err {
            ForjaError::NotConfigured(msg) => {
                assert!(msg.contains("claude-falso"), "mensaje: {msg}");
                assert!(msg.contains("no encontrado"), "mensaje: {msg}");
            }
            other => panic!("esperaba ForjaError::NotConfigured, obtuve: {other:?}"),
        }
    }
}
