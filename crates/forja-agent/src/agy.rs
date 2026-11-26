use std::path::PathBuf;

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
}
