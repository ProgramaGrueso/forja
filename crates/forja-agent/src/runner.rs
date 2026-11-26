use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use forja_core::{ForjaError, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// Canal de origen de un fragmento de salida.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

impl OutputStream {
    pub fn as_str(self) -> &'static str {
        match self {
            OutputStream::Stdout => "stdout",
            OutputStream::Stderr => "stderr",
        }
    }
}

/// Receptor de salida en vivo: se invoca una vez por línea (con su `\n` final si lo hubo).
pub type OutputSink = Arc<dyn Fn(OutputStream, String) + Send + Sync>;

tokio::task_local! {
    static CURRENT_SINK: OutputSink;
}

/// Ejecuta `fut` con un sink de salida activo: todo `Runner::run_argv*` lanzado
/// dentro de esta misma tarea (p. ej. el de un `Agent::run`) emite líneas al sink
/// sin que el agente tenga que saberlo.
pub async fn with_output_sink<F: std::future::Future>(sink: OutputSink, fut: F) -> F::Output {
    CURRENT_SINK.scope(sink, fut).await
}

/// Emite una línea al sink activo (si lo hay); para avisos propios del adaptador.
pub fn emit_to_sink(stream: OutputStream, text: &str) {
    if let Ok(sink) = CURRENT_SINK.try_with(Arc::clone) {
        sink(stream, text.to_string());
    }
}

async fn pump<R: AsyncRead + Unpin>(
    reader: R,
    stream: OutputStream,
    sink: Option<OutputSink>,
) -> std::io::Result<String> {
    let mut reader = BufReader::new(reader);
    let mut all = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            break;
        }
        all.extend_from_slice(&line);
        if let Some(sink) = &sink {
            sink(stream, String::from_utf8_lossy(&line).into_owned());
        }
    }
    Ok(String::from_utf8_lossy(&all).into_owned())
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

    /// Como `run_argv_with_env`; si hay un sink activo (`with_output_sink`) lo usa.
    pub async fn run_argv_with_env(
        argv: &[String],
        workdir: &Path,
        timeout_secs: u64,
        env: &[(&str, &str)],
    ) -> Result<CommandResult> {
        let sink = CURRENT_SINK.try_with(Arc::clone).ok();
        Self::run_argv_streaming(argv, workdir, timeout_secs, env, sink).await
    }

    /// Variante con salida en vivo: emite cada línea de stdout/stderr al `sink`
    /// mientras el proceso corre y devuelve igualmente la captura completa.
    pub async fn run_argv_streaming(
        argv: &[String],
        workdir: &Path,
        timeout_secs: u64,
        env: &[(&str, &str)],
        sink: Option<OutputSink>,
    ) -> Result<CommandResult> {
        if argv.is_empty() || argv[0].is_empty() {
            return Err(ForjaError::InvalidInput(
                "argv no puede estar vacío".to_string(),
            ));
        }

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn()?;
        let stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| ForjaError::Agent("sin pipe de stdout".to_string()))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| ForjaError::Agent("sin pipe de stderr".to_string()))?;

        let work = async {
            let (out, err, status) = tokio::join!(
                pump(stdout_pipe, OutputStream::Stdout, sink.clone()),
                pump(stderr_pipe, OutputStream::Stderr, sink.clone()),
                child.wait()
            );
            Ok::<_, std::io::Error>((out?, err?, status?))
        };

        let (stdout, stderr, status) =
            match tokio::time::timeout(Duration::from_secs(timeout_secs), work).await {
                Ok(res) => res?,
                Err(_) => {
                    return Err(ForjaError::Timeout {
                        seconds: timeout_secs,
                    });
                }
            };
        let exit_code = status.code();

        if !status.success() {
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

    #[tokio::test]
    async fn runner_streaming_emits_lines_while_running() {
        use std::sync::Mutex;
        use std::time::Instant;

        let dir = tempdir().expect("tempdir");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake_stream.sh");
        let argv = vec![script.to_string_lossy().into_owned()];
        let seen: Arc<Mutex<Vec<(OutputStream, String, Instant)>>> = Arc::default();
        let seen2 = Arc::clone(&seen);
        let sink: OutputSink = Arc::new(move |st, text| {
            seen2.lock().unwrap().push((st, text, Instant::now()));
        });
        let start = Instant::now();
        let res = Runner::run_argv_streaming(
            &argv,
            dir.path(),
            10,
            &[
                ("FAKE_STREAM_LINES", "3"),
                ("FAKE_STREAM_SLEEP", "0.2"),
                ("FAKE_STREAM_STDERR", "aviso"),
            ],
            Some(sink),
        )
        .await
        .expect("debe terminar bien");
        let total = start.elapsed();

        let seen = seen.lock().unwrap();
        let out: Vec<&str> = seen
            .iter()
            .filter(|(s, _, _)| *s == OutputStream::Stdout)
            .map(|(_, t, _)| t.as_str())
            .collect();
        assert_eq!(out, ["linea 1\n", "linea 2\n", "linea 3\n"]);
        assert!(seen
            .iter()
            .any(|(s, t, _)| *s == OutputStream::Stderr && t == "aviso\n"));
        // La primera línea llegó claramente antes de que el proceso terminara.
        assert!(total - seen[0].2.duration_since(start) >= Duration::from_millis(300));
        assert_eq!(res.stdout, "linea 1\nlinea 2\nlinea 3\n");
        assert_eq!(res.stderr, "aviso\n");
    }
}
