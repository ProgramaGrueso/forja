//! Regresiones del test adversarial (docs/adversarial/ENTREGA.md): A1, A2, M1,
//! M6, M7, M8, B2, B6 y la herencia del token, a través de la API.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use forja_agent::{Agent, MockAgent, Runner};
use forja_core::{AgentKind, Privacy};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

use super::{get, read_json, run_git, test_state, test_state_with_repo, wait_until_running};
use crate::settings::{load_settings_or_warn, save_settings};
use crate::{build_router, AppState};

async fn send(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .header(header::HOST, "127.0.0.1:7777")
        .header(header::CONTENT_TYPE, "application/json")
        .method(method)
        .uri(uri)
        .body(Body::from(body.to_string()))
        .expect("request");
    let res = app.clone().oneshot(req).await.expect("respuesta");
    let status = res.status();
    (status, read_json(res.into_body()).await)
}

async fn select(app: &axum::Router, path: &Path) -> (StatusCode, Value) {
    let body = json!({ "path": path.to_string_lossy() });
    send(app, Method::POST, "/api/workspaces/select", body).await
}

fn canon(path: &Path) -> String {
    path.canonicalize()
        .expect("canon")
        .to_string_lossy()
        .into_owned()
}

async fn write(app: &axum::Router, workspace: &str, path: &str) -> (StatusCode, Value) {
    let body = json!({ "path": path, "content": "ATAQUE\n", "workspace": workspace });
    send(app, Method::PUT, "/api/files/write", body).await
}

fn repo_state() -> (AppState, TempDir, TempDir) {
    let (state, repo, data) = test_state_with_repo();
    let state = state.with_workspaces_path(data.path().join("workspaces.json"));
    (state, repo, data)
}

fn link(target: &str, at: &Path) {
    std::os::unix::fs::symlink(target, at).expect("symlink");
}

#[tokio::test]
async fn a1_git_dir_cannot_be_written_through_links_case_or_separate_git_dir() {
    let (state, repo, _data) = repo_state();
    let root = repo.path();
    link(".git", &root.join("g"));
    link(".git/config", &root.join("cfg"));
    fs::create_dir_all(root.join(".GIT")).expect(".GIT");
    // Repo anidado con el directorio git real bajo otro nombre.
    run_git(
        root,
        &["init", "-q", "--separate-git-dir=almacen", "anidado"],
    );
    let config = fs::read_to_string(root.join(".git/config")).expect("config");
    let separate = fs::read_to_string(root.join("almacen/config")).expect("config separado");
    let workspace = canon(root);
    let app = build_router(state);

    for path in [
        "g/config",
        "g/nuevo",
        "cfg",
        ".git/config",
        ".GIT/config",
        "almacen/config",
        "almacen/nuevo",
        "anidado/.git",
    ] {
        let (status, body) = write(&app, &workspace, path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert!(
            body["error"].as_str().unwrap_or("").contains(".git"),
            "{path}: {body}"
        );
    }
    assert_eq!(
        fs::read_to_string(root.join(".git/config")).expect("config"),
        config
    );
    assert_eq!(
        fs::read_to_string(root.join("almacen/config")).expect("config"),
        separate
    );
    assert!(!root.join(".git/nuevo").exists());

    for dir in [".git", "g", ".git/refs", "almacen", ".GIT"] {
        let (status, body) = select(&app, &root.join(dir)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{dir}: {body}");
    }
}

#[tokio::test]
async fn a1_forja_data_cannot_be_written_by_the_files_api() {
    let (state, repo, _data) = repo_state();
    let root = repo.path();
    fs::create_dir_all(root.join(".forja/store")).expect(".forja");
    fs::create_dir_all(root.join("conf")).expect("conf");
    fs::write(root.join(".forja/store/settings.json"), "{}").expect("settings");
    fs::write(root.join("conf/mis-ajustes.json"), "{}").expect("ajustes");
    link(".forja/store", &root.join("s"));
    link("conf/mis-ajustes.json", &root.join("atajo.json"));
    let state = state.with_settings_path(root.join("conf/mis-ajustes.json"));
    let workspace = canon(root);
    let app = build_router(state);

    for path in [
        ".forja/store/settings.json",
        ".forja/store/nuevo",
        "s/settings.json",
        "conf/mis-ajustes.json",
        "atajo.json",
    ] {
        let (status, body) = write(&app, &workspace, path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
    }
    for file in [".forja/store/settings.json", "conf/mis-ajustes.json"] {
        assert_eq!(fs::read_to_string(root.join(file)).expect("leer"), "{}");
    }
    let (status, _) = select(&app, &root.join(".forja/store")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = write(&app, &workspace, "conf/otro.txt").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a2_forbidden_folders_cannot_be_selected_but_projects_and_startup_repo_can() {
    // El repo de arranque vive en un sitio que de otro modo estaría prohibido.
    let base = TempDir::new().expect("tmp");
    let startup = base.path().join(".forja/proyecto");
    fs::create_dir_all(startup.join("sub")).expect("startup");
    run_git(&startup, &["init", "-q", "-b", "main"]);
    let data = TempDir::new().expect("data");
    let mut config = forja_sched::SchedulerConfig::new(data.path().join("wt"));
    config.repo_path = Some(startup.clone());
    let store = forja_store::JsonlStore::new(data.path().join("store")).expect("store");
    let scheduler = forja_sched::Scheduler::new(store, config).expect("scheduler");

    let normal = TempDir::new().expect("normal");
    link("/etc", &normal.path().join("etc"));
    let home = std::env::var("HOME").expect("HOME");
    let home = Path::new(&home).canonicalize().expect("canon HOME");

    // Recientes guardados antes de esta restricción.
    let recent = data.path().join("workspaces.json");
    let saved = json!(["/", "/etc", home.to_string_lossy(), canon(normal.path())]);
    fs::write(&recent, saved.to_string()).expect("recientes");
    let app = build_router(AppState::new(scheduler).with_workspaces_path(recent));

    let list: Value = read_json(get(&app, "/api/workspaces").await.into_body()).await;
    let paths: Vec<&str> = list["recent"]
        .as_array()
        .expect("recent")
        .iter()
        .filter_map(|r| r["path"].as_str())
        .collect();
    assert_eq!(paths, [canon(normal.path())]);

    let mut forbidden = vec![
        Path::new("/").to_path_buf(),
        Path::new("/etc").to_path_buf(),
        Path::new("/proc").to_path_buf(),
        normal.path().join("etc"),
        home.clone(),
        startup.join("sub"),
    ];
    forbidden.extend(home.parent().map(Path::to_path_buf));
    for path in forbidden {
        let (status, body) = select(&app, &path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path:?}: {body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or("")
                .contains("no permitida"),
            "{body}"
        );
    }

    let (status, body) = select(&app, normal.path()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = select(&app, &startup).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["active"]["path"], canon(&startup));
}

#[tokio::test]
async fn a2_forbidden_initial_folder_is_not_served() {
    // Sin repo, arrancar en una carpeta prohibida no la expone por la API.
    let (mut state, _s, _w) = test_state();
    let data = TempDir::new().expect("data");
    state.workspaces = crate::workspaces::WorkspaceService::new(
        "/etc".into(),
        data.path().join("workspaces.json"),
    );
    let app = build_router(state);
    for uri in [
        "/api/files/tree",
        "/api/files/read?path=hostname",
        "/api/files/search?q=pass",
    ] {
        assert_eq!(
            get(&app, uri).await.status(),
            StatusCode::BAD_REQUEST,
            "{uri}"
        );
    }
    let (status, _) = write(&app, "/etc", "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn m7_write_requires_the_workspace_of_the_read() {
    let (state, repo, _data) = repo_state();
    let other = TempDir::new().expect("otro");
    fs::write(other.path().join("README.md"), "OTRO\n").expect("readme");
    let app = build_router(state);

    let read: Value = read_json(
        get(&app, "/api/files/read?path=README.md")
            .await
            .into_body(),
    )
    .await;
    let first = read["workspace"].as_str().expect("workspace").to_string();
    assert_eq!(first, canon(repo.path()));

    let missing = json!({ "path": "README.md", "content": "x" });
    let (status, body) = send(&app, Method::PUT, "/api/files/write", missing).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap_or("").contains("workspace"),
        "{body}"
    );

    let (status, _) = select(&app, other.path()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = write(&app, &first, "README.md").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        fs::read_to_string(other.path().join("README.md")).expect("leer"),
        "OTRO\n"
    );
    let (status, body) = write(&app, &canon(other.path()), "README.md").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn m8_saving_keeps_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (state, repo, _data) = repo_state();
    let workspace = canon(repo.path());
    let app = build_router(state);
    for (name, mode) in [("secreto.env", 0o600), ("script.sh", 0o755)] {
        let file = repo.path().join(name);
        fs::write(&file, "antes").expect("escribir");
        fs::set_permissions(&file, fs::Permissions::from_mode(mode)).expect("chmod");
        let (status, body) = write(&app, &workspace, name).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let got = fs::metadata(&file).expect("meta").permissions().mode() & 0o777;
        assert_eq!(got, mode, "{name}");
    }
}

#[tokio::test]
async fn b6_search_reports_truncation() {
    let (state, repo, _data) = repo_state();
    for n in 0..70 {
        fs::write(repo.path().join(format!("aguja{n}.txt")), "x").expect("escribir");
    }
    let app = build_router(state);
    let all: Value = read_json(get(&app, "/api/files/search?q=aguja").await.into_body()).await;
    assert_eq!(all["truncated"], true);
    let one: Value = read_json(get(&app, "/api/files/search?q=README").await.into_body()).await;
    assert_eq!(one["truncated"], false);
    assert_eq!(one["matches"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn m1_guarded_settings_cannot_change_while_a_run_is_active() {
    let (state, _repo, data) = repo_state();
    let settings_path = data.path().join("settings.json");
    let state = state.with_settings_path(settings_path.clone());
    let app = build_router(state.clone());
    let current: Value = read_json(get(&app, "/api/settings").await.into_body()).await;

    let slow: Arc<dyn Agent> =
        Arc::new(MockAgent::success("lento").with_delay(Duration::from_secs(10)));
    let run = state
        .scheduler
        .submit(forja_core::Task::new("larga", "p", Privacy::Open), slow)
        .await
        .expect("encolar");
    wait_until_running(&state, &run.id).await;

    let changes = [
        json!({ "sensitive_agents": ["claude", "opencode"] }),
        json!({ "quota_path": data.path().join("otra.json") }),
        json!({ "orchestrator_agent": "opencode" }),
    ];
    for change in &changes {
        let (status, body) = send(&app, Method::PUT, "/api/settings", change.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{change}: {body}");
        let mut whole = current.clone();
        for (key, value) in change.as_object().expect("objeto") {
            whole[key] = value.clone();
        }
        let (status, body) = send(&app, Method::PUT, "/api/settings", whole).await;
        assert_eq!(status, StatusCode::CONFLICT, "{change}: {body}");
    }
    assert_eq!(state.scheduler.sensitive_agents(), [AgentKind::Claude]);
    assert!(!settings_path.exists(), "un 409 no guarda nada");

    // Guardar sin cambiar esos campos sigue permitido.
    let (status, body) = send(&app, Method::PUT, "/api/settings", current.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let other = json!({ "hide_mascot": true });
    let (status, body) = send(&app, Method::PUT, "/api/settings", other).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    state.scheduler.cancel_run(&run.id).await.expect("cancelar");
    let (status, body) = send(&app, Method::PUT, "/api/settings", changes[0].clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        state.scheduler.sensitive_agents(),
        [AgentKind::Claude, AgentKind::Opencode]
    );
}

#[tokio::test]
async fn m6_quota_never_overwrites_a_foreign_file() {
    let (state, _repo, data) = repo_state();
    let target = data.path().join("claude-settings.json");
    let app = build_router(state);
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/settings",
        json!({ "quota_path": target }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let put = || send(&app, Method::PUT, "/api/quota", json!({ "providers": {} }));

    for foreign in [
        r#"{"permissions":{"allow":[]},"env":{}}"#,
        r#"{"providers":{},"theme":"dark"}"#,
        r#"{"providers":{"anthropic":{"apiKey":"x"}}}"#,
        r#"{"providers":{"mio":{}}}"#,
        r#"{"providers":[]}"#,
        "[1,2]",
        "no es json",
    ] {
        fs::write(&target, foreign).expect("escribir");
        let (status, body) = put().await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{foreign}: {body}");
        assert_eq!(fs::read_to_string(&target).expect("leer"), foreign);
    }
    // A través de un symlink se juzga el destino.
    let real = data.path().join("real.json");
    fs::write(&real, r#"{"a":1}"#).expect("escribir");
    fs::remove_file(&target).expect("borrar");
    std::os::unix::fs::symlink(&real, &target).expect("symlink");
    assert_eq!(put().await.0, StatusCode::BAD_REQUEST);
    assert_eq!(fs::read_to_string(&real).expect("leer"), r#"{"a":1}"#);
    fs::remove_file(&target).expect("borrar");

    // Inexistente, vacío y documento de cuota válido sí.
    assert_eq!(put().await.0, StatusCode::OK);
    fs::write(&target, "  \n").expect("vaciar");
    assert_eq!(put().await.0, StatusCode::OK);
    let valid = r#"{"providers":{"nous":{"percent_remaining":40,"updated_at":"hoy"}}}"#;
    fs::write(&target, valid).expect("válido");
    assert_eq!(put().await.0, StatusCode::OK);
}

#[tokio::test]
async fn b2_partial_put_keeps_omitted_fields() {
    let (state, _repo, data) = repo_state();
    let app = build_router(state.with_settings_path(data.path().join("settings.json")));
    let setup = json!({
        "hidden_profiles": ["privado"],
        "agent_models": { "claude": "sonnet" },
        "orchestrator_agent": "claude",
    });
    let (status, body) = send(&app, Method::PUT, "/api/settings", setup).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let partial = json!({ "max_concurrency": 3 });
    let (status, body) = send(&app, Method::PUT, "/api/settings", partial).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settings"]["hidden_profiles"], json!(["privado"]));
    assert_eq!(body["settings"]["agent_models"]["claude"], "sonnet");
    assert_eq!(body["settings"]["orchestrator_agent"], "claude");
    assert_eq!(body["settings"]["max_concurrency"], 3);

    // Un null explícito vacía un opcional; lo inválido sigue siendo 400.
    let clear = json!({ "orchestrator_agent": null });
    let (status, body) = send(&app, Method::PUT, "/api/settings", clear).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settings"]["orchestrator_agent"], Value::Null);
    for bad in [
        json!([]),
        json!("x"),
        json!({ "max_concurrency": "dos" }),
        json!({ "hidden_profiles": null }),
        json!({ "sensitive_agents": [] }),
    ] {
        let (status, body) = send(&app, Method::PUT, "/api/settings", bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
}

#[tokio::test]
async fn b2_corrupt_settings_warn_and_are_copied_before_overwriting() {
    let (state, _repo, data) = repo_state();
    let path = data.path().join("settings.json");
    let corrupt = r#"{"hidden_profiles":["privado"], roto"#;
    fs::write(&path, corrupt).expect("escribir");

    let (loaded, warning) = load_settings_or_warn(&path);
    assert!(loaded.is_none());
    let warning = warning.expect("aviso");
    assert!(warning.contains("settings.json"), "{warning}");
    let copy = data.path().join("settings.json.corrupt");
    assert_eq!(fs::read_to_string(&copy).expect("copia"), corrupt);

    let state = state
        .with_settings_path(path.clone())
        .with_settings_warning(Some(warning));
    let app = build_router(state);
    let view: Value = read_json(get(&app, "/api/settings").await.into_body()).await;
    assert!(view["warning"].as_str().unwrap_or("").contains("corrupt"));

    // Corrupto otra vez en caliente: se copia aparte antes de sobrescribir.
    let second = "otra basura";
    fs::write(&path, second).expect("corromper");
    let (status, body) = send(&app, Method::PUT, "/api/settings", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(fs::read_to_string(&copy).expect("copia"), corrupt);
    assert_eq!(
        fs::read_to_string(data.path().join("settings.json.corrupt.1")).expect("copia 2"),
        second
    );
    let (loaded, warning) = load_settings_or_warn(&path);
    assert!(warning.is_none());
    // Guardar sobre un archivo sano no crea más copias.
    assert_eq!(save_settings(&path, &loaded.expect("sano")), Ok(None));
}

#[tokio::test]
async fn auth_token_is_not_inherited_by_agent_processes() {
    // Se usa como "token" el valor de una variable que Cargo define en las pruebas.
    let value = std::env::var("CARGO_PKG_VERSION").expect("version");
    let (state, _s, _w) = test_state();
    let _state = state.with_auth_token(value);
    let out = Runner::run_argv(&["env".to_string()], &std::env::temp_dir(), 10)
        .await
        .expect("env");
    assert!(!out.stdout.contains("CARGO_PKG_VERSION="));
    assert!(out.stdout.contains("PATH="));
}
