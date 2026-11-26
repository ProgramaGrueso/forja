//! Ayudas para que Forja sea cómodo de usar sin configuración previa.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use forja_core::Result;

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Raíz del repo git que contiene el directorio actual, si existe.
pub fn detect_repo() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    git_out(&cwd, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// Rama actual del repo (None si HEAD está desacoplado o no es un repo).
pub fn current_branch(repo: &Path) -> Option<String> {
    git_out(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD")
}

/// Abre la URL en el navegador por defecto; si falla, no es un error.
pub fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// Agentes reales: (nombre, binario, cómo instalarlo/usarlo si falta).
const AGENTS: [(&str, &str, &str); 5] = [
    ("agy", "agy", "CLI de Gemini (agy)"),
    (
        "claude",
        "claude",
        "Claude Code: https://claude.com/claude-code",
    ),
    ("opencode", "opencode", "OpenCode: https://opencode.ai"),
    (
        "hermes",
        "hermes",
        "Hermes Agent: https://hermes-agent.nousresearch.com",
    ),
    ("local", "llm", "comando `llm` para modelos locales"),
];

fn line<W: Write>(out: &mut W, ok: bool, msg: String) -> Result<()> {
    writeln!(out, "[{}] {msg}", if ok { "ok" } else { "!!" })?;
    Ok(())
}

pub fn doctor<W: Write>(out: &mut W, repo: Option<&Path>, port: u16) -> Result<()> {
    let mut problems = 0;

    let git = on_path("git");
    if git.is_none() {
        problems += 1;
    }
    line(
        out,
        git.is_some(),
        match &git {
            Some(p) => format!("git: {}", p.display()),
            None => "git no encontrado: instálalo, Forja lo necesita para los worktrees".into(),
        },
    )?;

    let repo = repo.map(Path::to_path_buf).or_else(detect_repo);
    match &repo {
        Some(r) => line(
            out,
            true,
            format!(
                "repo: {} (rama {})",
                r.display(),
                current_branch(r).unwrap_or_else(|| "?".into())
            ),
        )?,
        None => line(
            out,
            true,
            "repo: no estás dentro de un repo git (usa --repo o entra a uno para ejecutar tareas)"
                .into(),
        )?,
    }

    writeln!(
        out,
        "Agentes (con 'mock' siempre puedes probar sin instalar nada):"
    )?;
    let mut found = 0;
    for (name, bin, hint) in AGENTS {
        match on_path(bin) {
            Some(p) => {
                found += 1;
                line(out, true, format!("{name}: {}", p.display()))?;
            }
            None => writeln!(out, "[--] {name}: no instalado ({hint})")?,
        }
    }
    if found == 0 {
        writeln!(
            out,
            "     Ningún agente real instalado: prueba `forja run --agent mock ...`."
        )?;
    }

    let free = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok();
    if !free {
        problems += 1;
    }
    line(
        out,
        free,
        if free {
            format!("puerto {port}: libre")
        } else {
            format!("puerto {port}: ocupado, usa `forja serve --port OTRO`")
        },
    )?;

    if problems == 0 {
        writeln!(out, "Todo listo. Siguiente paso: `forja serve --open`")?;
    } else {
        writeln!(out, "{problems} problema(s) por resolver arriba.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_reports_sections_and_succeeds() {
        let mut buf = Vec::new();
        doctor(&mut buf, None, 0).expect("doctor");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("git:") || text.contains("git no encontrado"));
        assert!(text.contains("Agentes"));
        assert!(text.contains("puerto 0"));
    }

    #[test]
    fn current_branch_of_non_repo_is_none() {
        let dir = std::env::temp_dir().join("forja-no-repo-xyz");
        let _ = std::fs::create_dir_all(&dir);
        assert!(current_branch(&dir).is_none());
    }
}
