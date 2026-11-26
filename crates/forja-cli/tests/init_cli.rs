//! `forja init` y `forja doctor` como binario real, con PATH, HOME y HERMES_HOME
//! falsos: nunca se lee el ~/.hermes real ni se ejecuta ningún agente.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

fn tmp() -> TempDir {
    // `store_dir` rechaza rutas con `.hermes`; /tmp evita depender de TMPDIR.
    TempDir::new_in("/tmp").expect("tempdir en /tmp")
}

fn fake_bin(dir: &Path, names: &[&str]) {
    fs::create_dir_all(dir).expect("bin dir");
    for n in names {
        fs::write(dir.join(n), "#!/bin/sh\nexit 0\n").expect("script falso");
    }
}

fn forja(bin_dir: &Path, home: &Path, hermes_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forja"))
        .args(args)
        .env_clear()
        .env("PATH", bin_dir)
        .env("HOME", home)
        .env("HERMES_HOME", hermes_home)
        .stdin(Stdio::null())
        .output()
        .expect("ejecutar forja")
}

#[test]
fn init_yes_writes_settings_then_doctor_stops_suggesting_init() {
    let root = tmp();
    let bin = root.path().join("bin");
    fake_bin(&bin, &["claude", "hermes"]);
    let hermes_home = root.path().join("hermes");
    fs::create_dir_all(hermes_home.join("profiles/uno")).expect("perfil falso");
    let store = root.path().join("store");
    let store_s = store.to_string_lossy().into_owned();

    let doc = forja(
        &bin,
        root.path(),
        &hermes_home,
        &["--store-dir", &store_s, "doctor"],
    );
    assert!(doc.status.success());
    let text = String::from_utf8_lossy(&doc.stdout);
    assert!(text.contains("ejecuta `forja init`"), "{text}");

    let out = forja(
        &bin,
        root.path(),
        &hermes_home,
        &["--store-dir", &store_s, "init", "--yes"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("[ok] claude") && text.contains("[--] agy"),
        "{text}"
    );
    assert!(text.contains("Para arrancar: forja --store-dir"), "{text}");

    let raw = fs::read_to_string(store.join("settings.json")).expect("settings.json");
    assert!(
        raw.contains("\"orchestrator_agent\": \"claude\""),
        "claude va primero en el orden automático: {raw}"
    );
    assert!(raw.contains("\"hidden_profiles\": []"), "{raw}");

    let doc = forja(
        &bin,
        root.path(),
        &hermes_home,
        &["--store-dir", &store_s, "doctor"],
    );
    assert!(!String::from_utf8_lossy(&doc.stdout).contains("forja init"));

    // Repetir `init --yes` conserva lo existente (merge, incluidos campos desconocidos).
    let edited = raw
        .replace("\"max_concurrency\": 2", "\"max_concurrency\": 5")
        .replace(
            "\"hidden_profiles\"",
            "\"campo_futuro\": true,\n  \"hidden_profiles\"",
        );
    fs::write(store.join("settings.json"), edited).expect("editar");
    let again = forja(
        &bin,
        root.path(),
        &hermes_home,
        &["--store-dir", &store_s, "init", "--yes"],
    );
    assert!(again.status.success());
    let raw = fs::read_to_string(store.join("settings.json")).expect("leer");
    assert!(raw.contains("\"max_concurrency\": 5"), "{raw}");
    assert!(raw.contains("\"campo_futuro\": true"), "{raw}");
}

#[test]
fn init_without_agents_still_succeeds() {
    let root = tmp();
    let bin = root.path().join("bin");
    fake_bin(&bin, &[]);
    let store = root.path().join("store");
    let out = forja(
        &bin,
        root.path(),
        &root.path().join("hermes"),
        &["--store-dir", &store.to_string_lossy(), "init", "--yes"],
    );
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("No hay ningún agente instalado"));
    assert!(store.join("settings.json").is_file());
}
