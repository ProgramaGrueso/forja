use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use forja_core::{ForjaError, Result};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

pub struct Runner;

impl Runner {
    pub async fn run_argv(
        argv: &[String],
        workdir: &Path,
        timeout_secs: u64,
    ) -> Result<CommandResult> {
        Self::run_argv_with_env(argv, workdir, timeout_secs, &[]).await
    }

    pub async fn run_argv_with_env(
        argv: &[String],
        workdir: &Path,
        timeout_secs: u64,
        env: &[(&str, &str)],
    ) -> Result<CommandResult> {
        if argv.is_empty() || argv[0].is_empty() {
            return Err(ForjaError::InvalidInput(
                "argv no puede estar vacío".to_string(),
            ));
        }

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(workdir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (key, value) in env {
            cmd.env(key, value);
        }

        let child = cmd.spawn()?;
        let output =
            match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
                .await
            {
                Ok(res) => res?,
                Err(_) => {
                    return Err(ForjaError::Timeout {
                        seconds: timeout_secs,
                    });
                }
            };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let exit_code = output.status.code();

        if !output.status.success() {
            return Err(ForjaError::Agent(format!(
                "comando '{}' falló con código {:?}; stderr: {}; stdout: {}",
                argv[0],
                exit_code,
                stderr.trim(),
                stdout.trim()
            )));
        }

        Ok(CommandResult {
            stdout,
            stderr,
            exit_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use forja_core::ForjaError;
    use tempfile::tempdir;

    use super::*;

    fn fake_agy_path() -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_agy.sh")
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn runner_empty_argv_returns_invalid_input() {
        let dir = tempdir().expect("tempdir");
        let err = Runner::run_argv(&[], dir.path(), 5)
            .await
            .expect_err("argv vacío debe fallar");
        assert!(matches!(err, ForjaError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn runner_fake_agy_success() {
        let dir = tempdir().expect("tempdir");
        let argv = vec![fake_agy_path()];
        let res = Runner::run_argv_with_env(&argv, dir.path(), 5, &[("FAKE_AGY_EXIT", "0")])
            .await
            .expect("fake_agy debe terminar con éxito");

        assert_eq!(res.exit_code, Some(0));
        let parsed: serde_json::Value =
            serde_json::from_str(res.stdout.trim()).expect("stdout debe ser JSON válido");
        assert_eq!(parsed["status"], "SUCCESS");
        assert_eq!(parsed["response"], "ok desde fake_agy");
    }

    #[tokio::test]
    async fn runner_fake_agy_nonzero_exit() {
        let dir = tempdir().expect("tempdir");
        let argv = vec![fake_agy_path()];
        let err = Runner::run_argv_with_env(
            &argv,
            dir.path(),
            5,
            &[
                ("FAKE_AGY_EXIT", "1"),
                ("FAKE_AGY_STDERR", "fallo simulado"),
            ],
        )
        .await
        .expect_err("fake_agy con código 1 debe fallar");

        match err {
            ForjaError::Agent(msg) => {
                assert!(msg.contains("1"), "esperaba código 1 en: {msg}");
                assert!(msg.contains("fallo simulado"), "esperaba stderr en: {msg}");
            }
            other => panic!("esperaba ForjaError::Agent, obtuve: {other:?}"),
        }
    }

    #[tokio::test]
    async fn runner_fake_agy_timeout() {
        let dir = tempdir().expect("tempdir");
        let argv = vec![fake_agy_path()];
        let err = Runner::run_argv_with_env(&argv, dir.path(), 1, &[("FAKE_AGY_SLEEP", "3")])
            .await
            .expect_err("fake_agy con sleep 3s y timeout 1s debe expirar");

        assert!(matches!(err, ForjaError::Timeout { seconds: 1 }));
    }
}
