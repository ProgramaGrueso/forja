//! Forja como app: `forja app` (arranca el panel en segundo plano y lo abre),
//! `forja app --stop/--status` y el lanzador de escritorio (`forja desktop ...`).
//!
//! Todas las rutas salen de [`AppEnv`], que se construye desde `HOME`/`XDG_*` pero
//! es inyectable para que los tests usen un HOME temporal.

use std::io::{BufRead, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use forja_core::{ForjaError, Result};
use serde_json::{json, Map, Value};

pub const DEFAULT_PORT: u16 = 7777;
const ICON_SOURCE: &str = "idle.webp";

fn io_err(err: std::io::Error) -> ForjaError {
    ForjaError::Io(err)
}

fn invalid(msg: impl Into<String>) -> ForjaError {
    ForjaError::InvalidInput(msg.into())
}

/// Rutas y programas que usa la app (todo inyectable).
#[derive(Debug, Clone)]
pub struct AppEnv {
    /// `$XDG_CONFIG_HOME` o `~/.config`.
    pub config_home: PathBuf,
    /// `$XDG_STATE_HOME` o `~/.local/state`.
    pub state_home: PathBuf,
    /// `$XDG_DATA_HOME` o `~/.local/share`.
    pub data_home: PathBuf,
    /// Binario `forja` a lanzar y a poner en `Exec=`.
    pub exe: PathBuf,
    /// Programa para abrir el navegador.
    pub opener: String,
    /// Cuánto esperar a que el servidor responda tras arrancarlo.
    pub start_timeout: Duration,
}

fn xdg(var: &str, home: &Path, fallback: &str) -> PathBuf {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => home.join(fallback),
    }
}

impl AppEnv {
    /// Entorno real: `HOME`, `XDG_*` y el ejecutable actual.
    pub fn detect() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
            .ok_or_else(|| {
                invalid("HOME no está definido: no sé dónde guardar la configuración")
            })?;
        let exe = std::env::current_exe().map_err(io_err)?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        Ok(Self {
            config_home: xdg("XDG_CONFIG_HOME", &home, ".config"),
            state_home: xdg("XDG_STATE_HOME", &home, ".local/state"),
            data_home: xdg("XDG_DATA_HOME", &home, ".local/share"),
            exe,
            opener: if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            }
            .to_string(),
            start_timeout: Duration::from_secs(20),
        })
    }

    /// Entorno bajo un HOME dado, sin mirar variables XDG (tests).
    pub fn from_home(home: &Path, exe: PathBuf) -> Self {
        Self {
            config_home: home.join(".config"),
            state_home: home.join(".local/state"),
            data_home: home.join(".local/share"),
            exe,
            opener: "xdg-open".to_string(),
            start_timeout: Duration::from_secs(20),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_home.join("forja/app.json")
    }
    pub fn state_dir(&self) -> PathBuf {
        self.state_home.join("forja")
    }
    pub fn pid_file(&self) -> PathBuf {
        self.state_dir().join("app.pid")
    }
    pub fn log_file(&self) -> PathBuf {
        self.state_dir().join("forja.log")
    }
    pub fn desktop_file(&self) -> PathBuf {
        self.data_home.join("applications/forja.desktop")
    }
    pub fn icon_file(&self) -> PathBuf {
        self.data_home.join("icons/forja.webp")
    }
}

// ---------------------------------------------------------------- app.json

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub repo: Option<PathBuf>,
    pub port: u16,
}

fn read_config_map(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(invalid(format!(
                "{} no es un JSON válido (objeto); corrígelo o bórralo",
                path.display()
            ))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(io_err(e)),
    }
}

pub fn load_config(env: &AppEnv) -> Result<AppConfig> {
    let map = read_config_map(&env.config_file())?;
    let repo = map
        .get("repo")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    let port = map
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p != 0)
        .unwrap_or(DEFAULT_PORT);
    Ok(AppConfig { repo, port })
}

/// Guarda `repo` y/o `port` fusionando con lo que ya hubiera en `app.json`
/// (las claves desconocidas se conservan).
pub fn save_config(env: &AppEnv, repo: Option<&Path>, port: Option<u16>) -> Result<AppConfig> {
    let path = env.config_file();
    let mut map = read_config_map(&path)?;
    if let Some(r) = repo {
        map.insert("repo".into(), json!(r.to_string_lossy()));
    }
    if let Some(p) = port {
        map.insert("port".into(), json!(p));
    } else if !map.contains_key("port") {
        map.insert("port".into(), json!(DEFAULT_PORT));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err)?;
    }
    let text =
        serde_json::to_string_pretty(&Value::Object(map)).map_err(|e| invalid(e.to_string()))?;
    std::fs::write(&path, format!("{text}\n")).map_err(io_err)?;
    load_config(env)
}

// ------------------------------------------------------------------ health

/// `true` si algo en 127.0.0.1:`port` contesta 200 a `GET /api/health` con el
/// JSON de Forja (`"status":"ok"`).
pub fn health_ok(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(800)));
    let req =
        format!("GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = String::new();
    let _ = stream.take(8192).read_to_string(&mut buf);
    buf.starts_with("HTTP/1.") && buf.contains(" 200") && buf.contains("\"status\":\"ok\"")
}

fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

// --------------------------------------------------------------------- pid

fn read_pid(env: &AppEnv) -> Option<u32> {
    std::fs::read_to_string(env.pid_file())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// El pid está vivo y es un `forja ... serve` (no un pid reciclado por otro programa).
pub fn pid_is_forja(pid: u32) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let args: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    let base = |a: &String| {
        Path::new(a)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    // arg0 = forja, o arg1 cuando forja es un script lanzado por un intérprete.
    let is_forja = args.iter().take(2).any(|a| base(a) == "forja");
    is_forja && args.iter().any(|a| a == "serve")
}

fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

// ------------------------------------------------------------------- start

fn open_url(env: &AppEnv, url: &str) {
    let _ = Command::new(&env.opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn is_git_repo(dir: &Path) -> bool {
    dir.is_dir()
        && Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--git-dir"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

/// `forja app`: arranca el servidor si hace falta y abre el panel.
/// `cwd_repo` es el repo git del directorio actual (si lo hay).
pub fn app_start<W: Write>(env: &AppEnv, out: &mut W, cwd_repo: Option<PathBuf>) -> Result<()> {
    let cfg = load_config(env)?;
    let repo = cfg
        .repo
        .clone()
        .filter(|r| is_git_repo(r))
        .or(cwd_repo)
        .ok_or_else(|| {
            invalid(
                "no hay repo configurado para la app: entra a un repo git o ejecuta `forja init` \
                 para elegir el repo por defecto",
            )
        })?;
    if !env.config_file().is_file() || cfg.repo.is_none() {
        // Crea app.json la primera vez (o recuerda el repo detectado).
        save_config(env, Some(&repo), Some(cfg.port))?;
    }
    let port = cfg.port;
    let panel = url(port);

    if health_ok(port) {
        writeln!(
            out,
            "Forja ya está en marcha en {panel}: no se lanza otra copia."
        )
        .map_err(io_err)?;
    } else {
        std::fs::create_dir_all(env.state_dir()).map_err(io_err)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(env.log_file())
            .map_err(io_err)?;
        let log_err = log.try_clone().map_err(io_err)?;
        let mut child = Command::new(&env.exe)
            .arg("serve")
            .arg("--repo")
            .arg(&repo)
            .arg("--port")
            .arg(port.to_string())
            .current_dir(&repo)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err)
            .process_group(0)
            .spawn()
            .map_err(io_err)?;
        std::fs::write(env.pid_file(), format!("{}\n", child.id())).map_err(io_err)?;
        let deadline = Instant::now() + env.start_timeout;
        loop {
            if health_ok(port) {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let _ = std::fs::remove_file(env.pid_file());
                return Err(ForjaError::Agent(format!(
                    "forja serve terminó antes de responder ({status}); revisa {}",
                    env.log_file().display()
                )));
            }
            if Instant::now() >= deadline {
                return Err(ForjaError::Agent(format!(
                    "el panel no respondió en {} s; revisa {}",
                    env.start_timeout.as_secs(),
                    env.log_file().display()
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        writeln!(
            out,
            "Forja arrancó en {panel} (repo: {}, log: {})",
            repo.display(),
            env.log_file().display()
        )
        .map_err(io_err)?;
    }
    open_url(env, &panel);
    Ok(())
}

// ------------------------------------------------------------- stop/status

pub fn app_status<W: Write>(env: &AppEnv, out: &mut W) -> Result<bool> {
    let cfg = load_config(env)?;
    let responding = health_ok(cfg.port);
    let pid = read_pid(env).filter(|p| pid_is_forja(*p));
    match (pid, responding) {
        (Some(p), true) => {
            writeln!(out, "Forja en marcha: pid {p}, {}", url(cfg.port)).map_err(io_err)?;
        }
        (Some(p), false) => {
            writeln!(
                out,
                "Forja (pid {p}) vive pero el puerto {} no responde.",
                cfg.port
            )
            .map_err(io_err)?;
        }
        (None, true) => {
            writeln!(
                out,
                "Hay un Forja respondiendo en {} que no lanzó `forja app` (no tengo su pid).",
                url(cfg.port)
            )
            .map_err(io_err)?;
        }
        (None, false) => writeln!(out, "Forja está detenido.").map_err(io_err)?,
    }
    writeln!(out, "  config: {}", env.config_file().display()).map_err(io_err)?;
    writeln!(out, "  log: {}", env.log_file().display()).map_err(io_err)?;
    Ok(pid.is_some() || responding)
}

pub fn app_stop<W: Write>(env: &AppEnv, out: &mut W) -> Result<()> {
    let Some(pid) = read_pid(env) else {
        writeln!(
            out,
            "No hay un Forja lanzado por `forja app` (sin pid guardado)."
        )
        .map_err(io_err)?;
        return Ok(());
    };
    if !pid_is_forja(pid) {
        let _ = std::fs::remove_file(env.pid_file());
        writeln!(
            out,
            "El pid {pid} guardado ya no es un proceso de forja: no lo toco (se borró el pid obsoleto)."
        )
        .map_err(io_err)?;
        return Ok(());
    }
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map_err(io_err)?;
    if !status.success() {
        return Err(ForjaError::Agent(format!("no pude detener el pid {pid}")));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_alive(pid) && pid_is_forja(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if pid_alive(pid) && pid_is_forja(pid) {
        return Err(ForjaError::Agent(format!(
            "forja (pid {pid}) sigue vivo tras SIGTERM; deténlo a mano"
        )));
    }
    let _ = std::fs::remove_file(env.pid_file());
    writeln!(out, "Forja detenido (pid {pid}).").map_err(io_err)?;
    Ok(())
}

// ----------------------------------------------------------------- desktop

fn exec_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars()
        .any(|c| c.is_whitespace() || "\"'\\`$&;()<>|*?#~=%".contains(c))
    {
        let esc: String = s
            .chars()
            .flat_map(|c| match c {
                '"' | '`' | '$' | '\\' => vec!['\\', c],
                c => vec![c],
            })
            .collect();
        format!("\"{esc}\"")
    } else {
        s.into_owned()
    }
}

/// Contenido de `forja.desktop`.
pub fn desktop_entry(exe: &Path, icon: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=Forja\n\
         Comment=Orquestador local de agentes de código (abre el panel)\n\
         Exec={} app\n\
         Icon={}\n\
         Terminal=false\n\
         Categories=Development;\n",
        exec_quote(exe),
        icon.display()
    )
}

pub fn desktop_install<W: Write>(env: &AppEnv, out: &mut W) -> Result<()> {
    let icon = forja_server::mascot_bytes(ICON_SOURCE)
        .ok_or_else(|| ForjaError::NotFound(ICON_SOURCE.to_string()))?;
    let icon_path = env.icon_file();
    let desktop_path = env.desktop_file();
    for p in [&icon_path, &desktop_path] {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(io_err)?;
        }
    }
    std::fs::write(&icon_path, icon).map_err(io_err)?;
    std::fs::write(&desktop_path, desktop_entry(&env.exe, &icon_path)).map_err(io_err)?;
    writeln!(out, "Lanzador instalado: {}", desktop_path.display()).map_err(io_err)?;
    writeln!(out, "Icono: {}", icon_path.display()).map_err(io_err)?;
    writeln!(
        out,
        "Busca 'Forja' en el lanzador de aplicaciones (puede requerir reabrirlo)."
    )
    .map_err(io_err)?;
    Ok(())
}

pub fn desktop_uninstall<W: Write>(env: &AppEnv, out: &mut W) -> Result<()> {
    let mut removed = 0;
    for p in [env.desktop_file(), env.icon_file()] {
        match std::fs::remove_file(&p) {
            Ok(()) => {
                removed += 1;
                writeln!(out, "Eliminado: {}", p.display()).map_err(io_err)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(e)),
        }
    }
    if removed == 0 {
        writeln!(out, "No había lanzador instalado.").map_err(io_err)?;
    }
    Ok(())
}

// -------------------------------------------------------------------- init

fn is_yes(answer: &str) -> bool {
    matches!(
        answer.trim().to_lowercase().as_str(),
        "s" | "si" | "sí" | "y" | "yes"
    )
}

/// Pasos de `forja init` para la app: repo por defecto y lanzador de escritorio.
/// `yes` acepta los valores por defecto (repo detectado/guardado; sin lanzador salvo
/// que `desktop` sea true).
pub fn init_app_steps<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    env: &AppEnv,
    yes: bool,
    desktop: bool,
    cwd_repo: Option<PathBuf>,
) -> Result<()> {
    let cfg = load_config(env)?;
    let default = cfg
        .repo
        .clone()
        .or(cwd_repo)
        .map(|p| p.to_string_lossy().into_owned());
    writeln!(out).map_err(io_err)?;
    match default {
        None => writeln!(
            out,
            "Repo por defecto de la app: ninguno (no estás en un repo git). \
             Vuelve a ejecutar `forja init` dentro de uno o edita {}.",
            env.config_file().display()
        )
        .map_err(io_err)?,
        Some(def) => {
            let answer = ask(input, out, yes, "Repo por defecto de `forja app`", &def)?;
            let path = PathBuf::from(&answer);
            if is_git_repo(&path) {
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                save_config(env, Some(&path), Some(cfg.port))?;
                writeln!(out, "  guardado en {}", env.config_file().display()).map_err(io_err)?;
            } else {
                writeln!(out, "  \"{answer}\" no es un repo git: no se guarda.").map_err(io_err)?;
            }
        }
    }
    let install = desktop
        || (!yes
            && is_yes(&ask(
                input,
                out,
                false,
                "¿Instalar el lanzador de escritorio ('Forja' en el menú de aplicaciones)? (s/n)",
                "n",
            )?));
    if install {
        desktop_install(env, out)?;
    } else {
        writeln!(out, "Lanzador de escritorio: no instalado (`forja init --desktop` o `forja desktop install`).")
            .map_err(io_err)?;
    }
    Ok(())
}

fn ask<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    yes: bool,
    question: &str,
    default: &str,
) -> Result<String> {
    if yes {
        writeln!(out, "{question} [{default}]: {default}").map_err(io_err)?;
        return Ok(default.to_string());
    }
    write!(out, "{question} [{default}]: ").map_err(io_err)?;
    out.flush().map_err(io_err)?;
    let mut line = String::new();
    let n = input.read_line(&mut line).map_err(io_err)?;
    let answer = line.trim();
    if n == 0 {
        writeln!(out).map_err(io_err)?;
    }
    Ok(if answer.is_empty() {
        default.to_string()
    } else {
        answer.to_string()
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    fn tmp() -> TempDir {
        TempDir::new_in("/tmp").expect("tempdir")
    }

    fn env_in(home: &Path, exe: &Path) -> AppEnv {
        let mut e = AppEnv::from_home(home, exe.to_path_buf());
        e.opener = "true".to_string();
        e.start_timeout = Duration::from_secs(2);
        e
    }

    /// Servidor falso: contesta lo mismo a todo (health de Forja o no).
    fn fake_server(body: &'static str) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut s = s;
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(body.as_bytes());
            }
        });
        port
    }

    const HEALTH: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"ok\",\"version\":\"0\"}";

    fn git_repo(dir: &Path) {
        let ok = Command::new("git")
            .args(["init", "-q"])
            .arg(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    #[test]
    fn paths_follow_home_layout() {
        let e = AppEnv::from_home(Path::new("/h"), "/x/forja".into());
        assert_eq!(e.config_file(), Path::new("/h/.config/forja/app.json"));
        assert_eq!(e.log_file(), Path::new("/h/.local/state/forja/forja.log"));
        assert_eq!(e.pid_file(), Path::new("/h/.local/state/forja/app.pid"));
        assert_eq!(
            e.desktop_file(),
            Path::new("/h/.local/share/applications/forja.desktop")
        );
    }

    #[test]
    fn app_json_is_created_and_merged_keeping_unknown_keys() {
        let home = tmp();
        let env = env_in(home.path(), Path::new("/x/forja"));
        assert_eq!(
            load_config(&env).unwrap(),
            AppConfig {
                repo: None,
                port: DEFAULT_PORT
            }
        );
        save_config(&env, Some(Path::new("/r/uno")), None).unwrap();
        let cfg = save_config(&env, None, Some(8123)).unwrap();
        assert_eq!(cfg.repo.as_deref(), Some(Path::new("/r/uno")));
        assert_eq!(cfg.port, 8123);
        // Clave ajena y repo previo sobreviven a un guardado parcial.
        let path = env.config_file();
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["tema"] = json!("oscuro");
        std::fs::write(&path, v.to_string()).unwrap();
        save_config(&env, Some(Path::new("/r/dos")), None).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["tema"], "oscuro");
        assert_eq!(v["repo"], "/r/dos");
        assert_eq!(v["port"], 8123);
        // JSON roto: error claro y no se pisa.
        std::fs::write(&path, "{roto").unwrap();
        assert!(save_config(&env, Some(Path::new("/r/x")), None).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{roto");
    }

    #[test]
    fn health_detects_forja_busy_port_and_ignores_others() {
        let ok = fake_server(HEALTH);
        assert!(health_ok(ok));
        let other = fake_server("HTTP/1.1 200 OK\r\n\r\nhola");
        assert!(!health_ok(other));
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = free.local_addr().unwrap().port();
        drop(free);
        assert!(!health_ok(p));
    }

    #[test]
    fn app_start_does_not_spawn_when_port_already_answers() {
        let home = tmp();
        let repo = tmp();
        git_repo(repo.path());
        let marker = home.path().join("lanzado");
        let exe = home.path().join("forja-falso.sh");
        std::fs::write(&exe, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = env_in(home.path(), &exe);
        let port = fake_server(HEALTH);
        save_config(&env, Some(repo.path()), Some(port)).unwrap();
        let mut out = Vec::new();
        app_start(&env, &mut out, None).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("ya está en marcha"), "{text}");
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "no debe lanzar un segundo servidor");
        assert!(!env.pid_file().exists());
    }

    #[test]
    fn app_start_without_repo_asks_for_init() {
        let home = tmp();
        let env = env_in(home.path(), Path::new("/x/forja"));
        let err = app_start(&env, &mut Vec::new(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("forja init"), "{err}");
    }

    #[test]
    fn app_start_reports_server_that_dies_with_log_hint() {
        let home = tmp();
        let repo = tmp();
        git_repo(repo.path());
        let exe = home.path().join("forja-roto.sh");
        std::fs::write(&exe, "#!/bin/sh\necho boom >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = env_in(home.path(), &exe);
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        save_config(&env, Some(repo.path()), Some(port)).unwrap();
        let err = app_start(&env, &mut Vec::new(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("forja.log"), "{err}");
        assert!(std::fs::read_to_string(env.log_file())
            .unwrap()
            .contains("boom"));
    }

    #[test]
    fn stop_never_kills_a_pid_that_is_not_forja() {
        let home = tmp();
        let env = env_in(home.path(), Path::new("/x/forja"));
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::create_dir_all(env.state_dir()).unwrap();
        std::fs::write(env.pid_file(), format!("{}\n", child.id())).unwrap();
        let mut out = Vec::new();
        app_stop(&env, &mut out).unwrap();
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("no es un proceso de forja"));
        assert!(
            child.try_wait().unwrap().is_none(),
            "sleep debe seguir vivo"
        );
        assert!(!env.pid_file().exists());
        child.kill().unwrap();
        let _ = child.wait();
        // Sin pid: mensaje, sin error.
        let mut out = Vec::new();
        app_stop(&env, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("sin pid"));
    }

    #[test]
    fn desktop_entry_content() {
        let text = desktop_entry(
            Path::new("/opt/bin/forja"),
            Path::new("/h/icons/forja.webp"),
        );
        for line in [
            "[Desktop Entry]",
            "Type=Application",
            "Name=Forja",
            "Exec=/opt/bin/forja app",
            "Icon=/h/icons/forja.webp",
            "Terminal=false",
            "Categories=Development;",
        ] {
            assert!(text.lines().any(|l| l == line), "falta {line}:\n{text}");
        }
        assert!(text.lines().any(|l| l.starts_with("Comment=")));
        let q = desktop_entry(Path::new("/mi dir/forja"), Path::new("/i.webp"));
        assert!(q.contains("Exec=\"/mi dir/forja\" app"), "{q}");
    }

    #[test]
    fn desktop_install_and_uninstall_in_temp_home() {
        let home = tmp();
        let env = env_in(home.path(), Path::new("/opt/bin/forja"));
        let mut out = Vec::new();
        desktop_install(&env, &mut out).unwrap();
        let desktop = std::fs::read_to_string(env.desktop_file()).unwrap();
        assert!(desktop.contains("Exec=/opt/bin/forja app"));
        assert!(desktop.contains(&format!("Icon={}", env.icon_file().display())));
        assert_eq!(
            std::fs::read(env.icon_file()).unwrap(),
            forja_server::mascot_bytes("idle.webp").unwrap()
        );
        // Idempotente.
        desktop_install(&env, &mut Vec::new()).unwrap();
        let mut out = Vec::new();
        desktop_uninstall(&env, &mut out).unwrap();
        assert!(!env.desktop_file().exists() && !env.icon_file().exists());
        let mut out = Vec::new();
        desktop_uninstall(&env, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No había"));
    }

    #[test]
    fn init_steps_save_repo_and_offer_desktop() {
        let home = tmp();
        let repo = tmp();
        git_repo(repo.path());
        let env = env_in(home.path(), Path::new("/opt/bin/forja"));
        // --yes: acepta el repo detectado, no instala lanzador.
        let mut out = Vec::new();
        init_app_steps(
            &mut Cursor::new(Vec::new()),
            &mut out,
            &env,
            true,
            false,
            Some(repo.path().to_path_buf()),
        )
        .unwrap();
        let saved = load_config(&env).unwrap().repo.unwrap();
        assert_eq!(saved, std::fs::canonicalize(repo.path()).unwrap());
        assert!(!env.desktop_file().exists());
        // Interactivo: repo por Enter, lanzador "s".
        let mut out = Vec::new();
        init_app_steps(
            &mut Cursor::new(b"\ns\n".to_vec()),
            &mut out,
            &env,
            false,
            false,
            None,
        )
        .unwrap();
        assert!(
            env.desktop_file().is_file(),
            "{}",
            String::from_utf8_lossy(&out)
        );
        // --desktop sin preguntar; repo inválido no se guarda.
        desktop_uninstall(&env, &mut Vec::new()).unwrap();
        init_app_steps(
            &mut Cursor::new(b"/no/existe\n".to_vec()),
            &mut Vec::new(),
            &env,
            false,
            true,
            None,
        )
        .unwrap();
        assert!(env.desktop_file().is_file());
        assert_eq!(load_config(&env).unwrap().repo.unwrap(), saved);
    }
}
