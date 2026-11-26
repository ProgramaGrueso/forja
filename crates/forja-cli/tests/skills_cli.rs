//! `forja skills` y la oferta de `forja init` como binario real, con PATH, HOME y
//! HERMES_HOME falsos: nunca se toca el ~/.claude ni el ~/.hermes reales.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

fn tmp() -> TempDir {
    TempDir::new_in("/tmp").expect("tempdir en /tmp")
}

fn fake_bin(dir: &Path, names: &[&str]) {
    fs::create_dir_all(dir).expect("bin dir");
    for n in names {
        fs::write(dir.join(n), "#!/bin/sh\nexit 0\n").expect("script falso");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.join(n), fs::Permissions::from_mode(0o755)).expect("chmod");
        }
    }
}

struct Fx {
    root: TempDir,
}

impl Fx {
    fn new(agents: &[&str]) -> Self {
        let root = tmp();
        fake_bin(&root.path().join("bin"), agents);
        fs::create_dir_all(root.path().join("home")).expect("home");
        fs::create_dir_all(root.path().join("hermes/profiles/uno")).expect("perfil uno");
        fs::create_dir_all(root.path().join("hermes/profiles/secreto")).expect("perfil secreto");
        Self { root }
    }

    fn p(&self, rel: &str) -> std::path::PathBuf {
        self.root.path().join(rel)
    }

    fn run(&self, args: &[&str], stdin: &str, hidden: Option<&str>) -> Output {
        use std::io::Write;
        let store = self.p("store");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_forja"));
        cmd.arg("--store-dir")
            .arg(&store)
            .args(args)
            .env_clear()
            .env("PATH", self.p("bin"))
            .env("HOME", self.p("home"))
            .env("HERMES_HOME", self.p("hermes"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(h) = hidden {
            cmd.env("FORJA_HIDDEN_PROFILES", h);
        }
        let mut child = cmd.spawn().expect("forja");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("stdin");
        child.wait_with_output().expect("salida")
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn list_shows_embedded_skills() {
    let fx = Fx::new(&[]);
    let o = fx.run(&["skills", "list"], "", None);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    for n in [
        "forja-orquestador",
        "forja-revision",
        "systematic-debugging",
    ] {
        assert!(t.contains(n), "{t}");
    }
}

#[test]
fn install_is_dry_run_by_default_then_yes_copies_to_each_agent() {
    let fx = Fx::new(&["claude", "opencode", "agy", "hermes"]);
    let o = fx.run(
        &["skills", "install", "--skill", "forja-revision"],
        "",
        None,
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("DRY-RUN"), "{}", text(&o));
    assert!(!fx.p("home/.claude").exists());

    let o = fx.run(
        &["skills", "install", "--skill", "forja-revision", "--yes"],
        "",
        None,
    );
    assert!(o.status.success(), "{}", text(&o));
    for d in [
        "home/.claude/skills",
        "home/.config/opencode/skills",
        "home/.gemini/skills",
        "hermes/skills",
    ] {
        assert!(
            fx.p(d).join("forja-revision/SKILL.md").is_file(),
            "falta en {d}"
        );
        assert!(fx.p(d).join("forja-revision/.forja-skill").is_file());
    }

    let o = fx.run(&["skills", "status"], "", None);
    let t = text(&o);
    assert!(t.contains("[ok] forja-revision"), "{t}");
    assert!(t.contains("[--] spike"), "{t}");

    let o = fx.run(&["skills", "uninstall", "--yes"], "", None);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!fx.p("home/.claude/skills/forja-revision").exists());
    assert!(!fx.p("hermes/skills/forja-revision").exists());
}

#[test]
fn hermes_profile_goes_to_profile_dir_and_hidden_profiles_are_rejected() {
    let fx = Fx::new(&["hermes"]);
    let o = fx.run(
        &[
            "skills",
            "install",
            "--agent",
            "hermes",
            "--profile",
            "uno",
            "--skill",
            "spike",
            "--yes",
        ],
        "",
        None,
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(fx.p("hermes/profiles/uno/skills/spike/SKILL.md").is_file());

    let o = fx.run(
        &[
            "skills",
            "install",
            "--agent",
            "hermes",
            "--profile",
            "secreto",
            "--yes",
        ],
        "",
        Some("secreto"),
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("oculto"), "{}", text(&o));
    assert!(!fx.p("hermes/profiles/secreto/skills").exists());

    // También oculto vía settings.json del store.
    let o = fx.run(&["init", "--yes"], "", None);
    assert!(o.status.success(), "{}", text(&o));
    let path = fx.p("store/settings.json");
    let raw = fs::read_to_string(&path).expect("settings");
    let patched = raw.replacen(
        "\"hidden_profiles\": []",
        "\"hidden_profiles\": [\"secreto\"]",
        1,
    );
    assert_ne!(raw, patched, "formato inesperado de settings.json: {raw}");
    fs::write(&path, patched).expect("settings");
    let o = fx.run(
        &[
            "skills",
            "status",
            "--agent",
            "hermes",
            "--profile",
            "secreto",
        ],
        "",
        None,
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("oculto"), "{}", text(&o));
}

#[test]
fn init_offers_skills_as_dry_run_and_installs_only_on_yes_answer() {
    let fx = Fx::new(&["claude"]);
    // --yes: muestra el dry-run pero no instala.
    let o = fx.run(&["init", "--yes"], "", None);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("DRY-RUN"), "{}", text(&o));
    assert!(!fx.p("home/.claude").exists());

    // Interactivo respondiendo que no (Enter en todo y `n` al final).
    let o = fx.run(&["init"], "\n\n\nn\n", None);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!fx.p("home/.claude").exists());

    // Interactivo respondiendo que sí.
    let o = fx.run(&["init"], "\n\n\ns\n", None);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        fx.p("home/.claude/skills/forja-orquestador/SKILL.md")
            .is_file(),
        "{}",
        text(&o)
    );
}
