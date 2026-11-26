//! `forja app` / `forja desktop` como binario real con HOME falso: arranca un
//! servidor de verdad en localhost, comprueba que no se duplica y lo detiene.
//! `xdg-open` es un script falso que registra la URL; no se toca el HOME real.

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

fn forja(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forja"))
        .args(args)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("HOME", root.join("home"))
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .expect("ejecutar forja")
}

#[test]
fn app_start_is_idempotent_and_stop_works() {
    let root = TempDir::new_in("/tmp").unwrap();
    let r = root.path();
    fs::create_dir_all(r.join("bin")).unwrap();
    fs::create_dir_all(r.join("home")).unwrap();
    let opener = r.join("bin/xdg-open");
    fs::write(
        &opener,
        format!(
            "#!/bin/sh\necho \"$1\" >> {}\n",
            r.join("opened.txt").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&opener, fs::Permissions::from_mode(0o755)).unwrap();
    let repo = r.join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .status()
        .unwrap()
        .success());
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let cfg = r.join("home/.config/forja");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(
        cfg.join("app.json"),
        format!("{{\"repo\":\"{}\",\"port\":{port}}}", repo.display()),
    )
    .unwrap();

    let st = forja(r, &["app", "--status"]);
    assert!(String::from_utf8_lossy(&st.stdout).contains("detenido"));

    let out = forja(r, &["app"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pid1 = fs::read_to_string(r.join("home/.local/state/forja/app.pid")).unwrap();
    let st = forja(r, &["app", "--status"]);
    let text = String::from_utf8_lossy(&st.stdout).into_owned();
    assert!(
        text.contains("en marcha") && text.contains(pid1.trim()),
        "{text}"
    );

    // Segunda vez: no duplica.
    let out = forja(r, &["app"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("ya está en marcha"));
    assert_eq!(
        fs::read_to_string(r.join("home/.local/state/forja/app.pid")).unwrap(),
        pid1
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let opened = fs::read_to_string(r.join("opened.txt")).unwrap();
    assert_eq!(opened.lines().count(), 2, "{opened}");
    assert!(opened.contains(&format!("http://127.0.0.1:{port}")));
    assert!(r.join("home/.local/state/forja/forja.log").is_file());

    let out = forja(r, &["app", "--stop"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("detenido"),
        "{out:?}"
    );
    let st = forja(r, &["app", "--status"]);
    assert!(String::from_utf8_lossy(&st.stdout).contains("detenido"));
}

#[test]
fn desktop_install_uninstall_and_init_desktop_use_fake_home() {
    let root = TempDir::new_in("/tmp").unwrap();
    let r = root.path();
    fs::create_dir_all(r.join("home")).unwrap();
    let apps = r.join("home/.local/share/applications/forja.desktop");
    let out = forja(r, &["desktop", "install"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(&apps).unwrap();
    assert!(
        text.contains(&format!("Exec={} app", env!("CARGO_BIN_EXE_forja"))),
        "{text}"
    );
    assert!(r.join("home/.local/share/icons/forja.webp").is_file());
    forja(r, &["desktop", "uninstall"]);
    assert!(!apps.exists());

    let store = r.join("store");
    let out = forja(
        r,
        &[
            "--store-dir",
            &store.to_string_lossy(),
            "init",
            "--yes",
            "--desktop",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(apps.is_file());
}
