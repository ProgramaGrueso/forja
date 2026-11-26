use std::path::PathBuf;

use forja_core::AgentKind;
use serde_json::Value;

use crate::{Agent, AgentFuture, AgentOutput, RunContext, Runner};

pub const DEFAULT_MODEL: &str = "gemini-4-neon-medium";
pub const DEFAULT_MODE: &str = "auto";
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyCommandOptions {
    pub prompt: String,
    pub model: String,
    pub mode: String,
    pub timeout_secs: u64,
    pub worktree: PathBuf,
}

impl AgyCommandOptions {
    pub fn new(prompt: impl Into<String>, worktree: impl Into<PathBuf>) -> Self {
        Self {
            prompt: prompt.into(),
            model: DEFAULT_MODEL.to_string(),
            mode: DEFAULT_MODE.to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            worktree: worktree.into(),
        }
    }
}

pub fn build_command(opts: &AgyCommandOptions) -> Vec<String> {
    vec![
        "agy".to_string(),
        "-p".to_string(),
        opts.prompt.clone(),
        "--model".to_string(),
        opts.model.clone(),
        "--mode".to_string(),
        opts.mode.clone(),
        "--output-format".to_string(),
        "json".to_string(),
        "--print-timeout".to_string(),
        format!("{}s", opts.timeout_secs),
        "--add-dir".to_string(),
        opts.worktree.to_string_lossy().into_owned(),
    ]
}

const TEXT_FIELDS: [&str; 4] = ["response", "text", "output", "message"];

fn extract_text_from_value(value: &Value) -> Option<String> {
    let obj = value.as_object()?;
    for field in TEXT_FIELDS {
        if let Some(text) = obj.get(field).and_then(Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

pub fn parse_agy_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> AgentOutput {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return AgentOutput {
            text: String::new(),
            raw_stdout: stdout.to_string(),
            raw_stderr: stderr.to_string(),
            exit_code,
            warnings: vec!["salida vacía de agy".to_string()],
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
            text: raw_json.to_string(),
            raw_stdout: stdout.to_string(),
            raw_stderr: stderr.to_string(),
            exit_code,
            warnings: vec![
                "el JSON de salida de agy no contiene campos conocidos (response, text, output, message)"
                    .to_string(),
            ],
        };
    }

    AgentOutput {
        text: trimmed.to_string(),
        raw_stdout: stdout.to_string(),
        raw_stderr: stderr.to_string(),
        exit_code,
        warnings: vec!["la salida de agy no es JSON válido; se devuelve texto crudo".to_string()],
    }
}

#[derive(Debug, Clone)]
pub struct AgyAgent {
    pub binary: String,
    pub model: String,
    pub mode: String,
}

impl Default for AgyAgent {
    fn default() -> Self {
        Self::new(DEFAULT_MODEL, DEFAULT_MODE)
    }
}

impl AgyAgent {
    pub fn new(model: impl Into<String>, mode: impl Into<String>) -> Self {
        Self {
            binary: "agy".to_string(),
            model: model.into(),
            mode: mode.into(),
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }
}

impl Agent for AgyAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Agy
    }

    fn run<'a>(&'a self, ctx: &'a RunContext) -> AgentFuture<'a> {
        Box::pin(async move {
            let opts = AgyCommandOptions {
                prompt: ctx.task.prompt.clone(),
                model: self.model.clone(),
                mode: self.mode.clone(),
                timeout_secs: ctx.timeout_secs,
                worktree: ctx.worktree.clone(),
            };
            let mut argv = build_command(&opts);
            argv[0] = self.binary.clone();

            let res = Runner::run_argv(&argv, &ctx.worktree, ctx.timeout_secs).await?;
            Ok(parse_agy_output(&res.stdout, &res.stderr, res.exit_code))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agy_args_basic_order() {
        let opts = AgyCommandOptions::new("implementa T07", "/tmp/worktree");
        let args = build_command(&opts);

        assert_eq!(
            args,
            vec![
                "agy",
                "-p",
                "implementa T07",
                "--model",
                "gemini-4-neon-medium",
                "--mode",
                "auto",
                "--output-format",
                "json",
                "--print-timeout",
                "1800s",
                "--add-dir",
                "/tmp/worktree",
            ]
        );
    }

    #[test]
    fn agy_args_prompt_with_quotes() {
        let prompt = r#"Di "hola" 'mundo' sin escapar"#;
        let opts = AgyCommandOptions::new(prompt, "/tmp/wt");
        let args = build_command(&opts);

        assert_eq!(args.len(), 13);
        assert_eq!(args[1], "-p");
        assert_eq!(args[2], prompt);
    }

    #[test]
    fn agy_args_multiline_prompt_with_shell_metacharacters() {
        let prompt = "linea 1: $HOME\nlinea 2: `whoami`\nlinea 3: ; rm -rf /";
        let opts = AgyCommandOptions::new(prompt, "/tmp/wt");
        let args = build_command(&opts);

        assert_eq!(args.len(), 13);
        assert_eq!(args[1], "-p");
        assert_eq!(args[2], prompt);
    }

    #[test]
    fn agy_args_print_timeout_suffix() {
        let default_opts = AgyCommandOptions::new("p", "/tmp/wt");
        let default_args = build_command(&default_opts);
        assert_eq!(default_args[9], "--print-timeout");
        assert_eq!(default_args[10], "1800s");

        let mut custom_opts = AgyCommandOptions::new("p", "/tmp/wt");
        custom_opts.timeout_secs = 45;
        let custom_args = build_command(&custom_opts);
        assert_eq!(custom_args[9], "--print-timeout");
        assert_eq!(custom_args[10], "45s");
    }

    #[test]
    fn agy_args_worktree_with_spaces() {
        let path = PathBuf::from("/tmp/mi proyecto/worktree T07");
        let opts = AgyCommandOptions::new("p", path);
        let args = build_command(&opts);

        assert_eq!(args.len(), 13);
        assert_eq!(args[11], "--add-dir");
        assert_eq!(args[12], "/tmp/mi proyecto/worktree T07");
    }

    #[test]
    fn parse_agy_valid_json() {
        let stdout = r#"{"status":"SUCCESS","response":"tarea completada"}"#;
        let out = parse_agy_output(stdout, "", Some(0));

        assert_eq!(out.text, "tarea completada");
        assert_eq!(out.raw_stdout, stdout);
        assert_eq!(out.raw_stderr, "");
        assert_eq!(out.exit_code, Some(0));
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn parse_agy_broken_json() {
        let stdout = "{roto";
        let out = parse_agy_output(stdout, "algún aviso", Some(0));

        assert_eq!(out.text, "{roto");
        assert_eq!(out.raw_stdout, stdout);
        assert_eq!(out.raw_stderr, "algún aviso");
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn parse_agy_empty_output() {
        let out = parse_agy_output("   \n\t  ", "", Some(0));

        assert!(out.text.is_empty());
        assert_eq!(out.raw_stdout, "   \n\t  ");
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn parse_agy_json_after_warning_line() {
        let stdout =
            "WARN: deprecation notice\n{\"status\":\"SUCCESS\",\"response\":\"ok tras aviso\"}\n";
        let out = parse_agy_output(stdout, "", Some(0));

        assert_eq!(out.text, "ok tras aviso");
        assert_eq!(out.raw_stdout, stdout);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn parse_agy_json_without_known_fields() {
        let stdout = r#"{"status":"SUCCESS","other":123}"#;
        let out = parse_agy_output(stdout, "", Some(0));

        assert_eq!(out.text, stdout);
        assert!(!out.warnings.is_empty());
    }

    #[tokio::test]
    async fn agy_agent_runs_fake_agy() {
        use forja_core::{Privacy, Task};
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let fake_agy = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_agy.sh")
            .to_string_lossy()
            .into_owned();

        let agent = AgyAgent::default().with_binary(fake_agy);
        assert_eq!(agent.kind(), AgentKind::Agy);

        let ctx = RunContext {
            task: Task {
                id: "T09".to_string(),
                titulo: "Prueba AgyAgent".to_string(),
                prompt: "hola".to_string(),
                privacidad: Privacy::Open,
            },
            worktree: dir.path().to_path_buf(),
            timeout_secs: 5,
        };

        let out = agent.run(&ctx).await.expect("fake_agy debe responder ok");
        assert_eq!(out.text, "ok desde fake_agy");
        assert_eq!(out.exit_code, Some(0));
        assert!(out.warnings.is_empty());
    }

    #[tokio::test]
    #[ignore = "requiere binario agy real y cuota activa"]
    async fn agy_real_trivial_prompt_in_temp_worktree() {
        use forja_core::{Privacy, Task};
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let agent = AgyAgent::new("gemini-4-neon-low", "auto");
        let ctx = RunContext {
            task: Task::new(
                "X1 agy real",
                "Responde únicamente con la palabra OK y no ejecutes ninguna herramienta.",
                Privacy::Open,
            ),
            worktree: dir.path().to_path_buf(),
            timeout_secs: 90,
        };

        let out = agent.run(&ctx).await.expect("agy real debe responder");
        assert_eq!(out.exit_code, Some(0));
        assert!(
            !out.text.trim().is_empty(),
            "la respuesta de agy real no debe estar vacía"
        );
    }
}
