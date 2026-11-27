use std::fs;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use forja_agent::{Agent, MockAgent};
use forja_core::Privacy;
use tempfile::TempDir;
use tower::ServiceExt;

use crate::build_router;
use crate::files::{FileReadResponse, FileSearchResponse, FileTreeResponse, FileWriteResponse};
use crate::workspaces::{BrowseResponse, WorkspacesResponse};

use super::{
    finished_task, get, init_git_repo, read_json, run_git, test_state_with_repo, wait_until_running,
};

async fn post_json(
    app: &axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .header(header::CONTENT_TYPE, "application/json")
                .method(Method::POST)
                .uri(uri)
                .body(Body::from(body.to_string()))
                .expect("request POST JSON"),
        )
        .await
        .expect("respuesta POST JSON")
}

async fn put_json(
    app: &axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .header(header::HOST, "127.0.0.1:7777")
                .header(header::CONTENT_TYPE, "application/json")
                .method(Method::PUT)
                .uri(uri)
                .body(Body::from(body.to_string()))
                .expect("request PUT JSON"),
        )
        .await
        .expect("respuesta PUT JSON")
}

#[tokio::test]
async fn files_tree_read_write_and_search() {
    let (state, repo, data) = test_state_with_repo();
    let state = state.with_workspaces_path(data.path().join("workspaces.json"));

    fs::create_dir_all(repo.path().join("src")).expect("crear src");
    fs::create_dir_all(repo.path().join("target")).expect("crear target");
    fs::create_dir_all(repo.path().join(".github")).expect("crear .github");
    fs::write(repo.path().join("src/main.rs"), "fn main() {}\n").expect("escribir main.rs");
    fs::write(repo.path().join("target/ignored.txt"), "ignorado").expect("escribir target");
    fs::write(repo.path().join(".gitignore"), "target/\n").expect("escribir .gitignore");

    let app = build_router(state);

    // 1. Tree en raíz: omite .git y target, mantiene .github y .gitignore, carpetas primero.
    let res = get(&app, "/api/files/tree").await;
    assert_eq!(res.status(), StatusCode::OK);
    let tree: FileTreeResponse = read_json(res.into_body()).await;
    assert!(!tree.read_only);
    let names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(!names.contains(&".git"));
    assert!(!names.contains(&"target"));
    assert!(names.contains(&".github"));
    assert!(names.contains(&".gitignore"));
    assert!(names.contains(&"src"));
    assert!(names.contains(&"README.md"));
    // Carpetas antes que archivos
    let first_file_idx = tree
        .entries
        .iter()
        .position(|e| !e.is_dir)
        .expect("debe haber archivos");
    assert!(tree.entries[..first_file_idx].iter().all(|e| e.is_dir));

    // 2. Read archivo Rust
    let res = get(&app, "/api/files/read?path=src/main.rs").await;
    assert_eq!(res.status(), StatusCode::OK);
    let file: FileReadResponse = read_json(res.into_body()).await;
    assert_eq!(file.path, "src/main.rs");
    assert_eq!(file.content, "fn main() {}\n");
    assert_eq!(file.language, "rust");
    assert!(!file.read_only);

    // 3. Write archivo existente y nuevo (también > 256 KiB para verificar body limit de 1 MiB)
    let large_content = "a".repeat(300 * 1024);
    let res = put_json(
        &app,
        "/api/files/write",
        serde_json::json!({ "path": "src/big.txt", "content": large_content }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let written: FileWriteResponse = read_json(res.into_body()).await;
    assert!(written.ok);
    assert_eq!(written.path, "src/big.txt");
    assert_eq!(written.size, 300 * 1024);

    let res = get(&app, "/api/files/read?path=src/big.txt").await;
    assert_eq!(res.status(), StatusCode::OK);
    let read_back: FileReadResponse = read_json(res.into_body()).await;
    assert_eq!(read_back.size, 300 * 1024);

    // 4. Search
    let res = get(&app, "/api/files/search?q=MAIN").await;
    assert_eq!(res.status(), StatusCode::OK);
    let search: FileSearchResponse = read_json(res.into_body()).await;
    assert_eq!(search.matches.len(), 1);
    assert_eq!(search.matches[0].path, "src/main.rs");
}

#[tokio::test]
async fn files_security_rejects_traversal_symlinks_git_and_binary() {
    let (state, repo, data) = test_state_with_repo();
    let state = state.with_workspaces_path(data.path().join("workspaces.json"));

    let outside = TempDir::new().expect("tempdir externo");
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "secreto").expect("escribir secreto");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&secret, repo.path().join("escape_link.txt"))
            .expect("crear symlink archivo");
        std::os::unix::fs::symlink(outside.path(), repo.path().join("escape_dir"))
            .expect("crear symlink dir");
    }

    fs::write(repo.path().join("binario.dat"), [0u8, 1, 2, 255, 0]).expect("escribir binario");

    let app = build_router(state);

    // Path traversal y rutas absolutas en read y tree
    for uri in [
        "/api/files/read?path=../secret.txt",
        "/api/files/read?path=/etc/passwd",
        "/api/files/tree?path=..",
        "/api/files/tree?path=/etc",
    ] {
        let res = get(&app, uri).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "uri: {uri}");
    }

    // Path traversal y .git en write
    for bad_path in ["../fuera.txt", "/tmp/fuera.txt", ".git/config"] {
        let res = put_json(
            &app,
            "/api/files/write",
            serde_json::json!({ "path": bad_path, "content": "x" }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "path: {bad_path}");
    }

    // Symlink hacia fuera rechazado en read, tree y write
    #[cfg(unix)]
    {
        let res = get(&app, "/api/files/read?path=escape_link.txt").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let res = get(&app, "/api/files/tree?path=escape_dir").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let res = put_json(
            &app,
            "/api/files/write",
            serde_json::json!({ "path": "escape_link.txt", "content": "hack" }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    // Archivo binario rechazado con 400
    let res = get(&app, "/api/files/read?path=binario.dat").await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err: serde_json::Value = read_json(res.into_body()).await;
    assert!(
        err["error"]
            .as_str()
            .unwrap_or_default()
            .contains("binario"),
        "error inesperado: {err}"
    );
}

#[tokio::test]
async fn files_read_worktree_with_run_id_is_read_only() {
    let (state, _repo, data) = test_state_with_repo();
    let state = state.with_workspaces_path(data.path().join("workspaces.json"));
    let run = finished_task(&state, "crear nota", "nota.md", "hola worktree\n").await;

    let app = build_router(state);

    let res = get(&app, &format!("/api/files/tree?run_id={}", run.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let tree: FileTreeResponse = read_json(res.into_body()).await;
    assert!(tree.read_only);
    assert!(tree.entries.iter().any(|e| e.name == "nota.md"));

    let res = get(
        &app,
        &format!("/api/files/read?path=nota.md&run_id={}", run.id),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let file: FileReadResponse = read_json(res.into_body()).await;
    assert!(file.read_only);
    assert_eq!(file.content, "hola worktree\n");
    assert_eq!(file.language, "markdown");
}

#[tokio::test]
async fn workspaces_get_browse_select_and_conflict_on_active_run() {
    let (state, repo1, data) = test_state_with_repo();
    let workspaces_file = data.path().join("nested").join("workspaces.json");
    let state = state.with_workspaces_path(workspaces_file.clone());

    // Segundo repo git con rama "develop"
    let repo2 = init_git_repo();
    run_git(repo2.path(), &["checkout", "-b", "develop"]);

    // Carpeta normal con subcarpetas para browse
    let browse_root = TempDir::new().expect("crear browse tempdir");
    fs::create_dir(browse_root.path().join("alpha")).expect("alpha");
    fs::create_dir(browse_root.path().join("beta")).expect("beta");
    fs::create_dir(browse_root.path().join(".hidden")).expect(".hidden");

    let app = build_router(state.clone());

    // GET /api/workspaces inicial
    let res = get(&app, "/api/workspaces").await;
    assert_eq!(res.status(), StatusCode::OK);
    let initial: WorkspacesResponse = read_json(res.into_body()).await;
    assert_eq!(
        initial.active.path,
        repo1.path().canonicalize().unwrap().to_string_lossy()
    );
    assert!(initial.active.is_git);
    assert_eq!(initial.active.branch.as_deref(), Some("main"));
    assert!(initial.recent.is_empty());

    // GET /api/workspaces/browse sin ocultos
    let res = get(
        &app,
        &format!(
            "/api/workspaces/browse?path={}",
            browse_root.path().to_string_lossy()
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let browsed: BrowseResponse = read_json(res.into_body()).await;
    let names: Vec<&str> = browsed.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta"]);

    // GET /api/workspaces/browse con show_hidden=true
    let res = get(
        &app,
        &format!(
            "/api/workspaces/browse?path={}&show_hidden=true",
            browse_root.path().to_string_lossy()
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let browsed_hidden: BrowseResponse = read_json(res.into_body()).await;
    assert!(browsed_hidden.entries.iter().any(|e| e.name == ".hidden"));

    // POST /api/workspaces/select cambia a repo2, detecta rama develop y persiste recientes
    let res = post_json(
        &app,
        "/api/workspaces/select",
        serde_json::json!({ "path": repo2.path().to_string_lossy() }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let selected: WorkspacesResponse = read_json(res.into_body()).await;
    let repo2_canon = repo2
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(selected.active.path, repo2_canon);
    assert!(selected.active.is_git);
    assert_eq!(selected.active.branch.as_deref(), Some("develop"));
    assert_eq!(selected.recent.len(), 1);
    assert_eq!(selected.recent[0].path, repo2_canon);
    assert_eq!(state.scheduler.base_branch(), "develop");
    assert_eq!(
        state
            .scheduler
            .repo_path()
            .map(|p| p.to_string_lossy().into_owned()),
        Some(repo2_canon)
    );
    assert!(workspaces_file.exists());

    // Mientras hay una ejecución activa, POST /api/workspaces/select devuelve 409 Conflict
    let slow_agent: Arc<dyn Agent> =
        Arc::new(MockAgent::success("lento").with_delay(Duration::from_secs(5)));
    let running = state
        .scheduler
        .submit(
            forja_core::Task::new("larga", "prompt", Privacy::Open),
            slow_agent,
        )
        .await
        .expect("encolar tarea lenta");
    let _ = wait_until_running(&state, &running.id).await;

    let res = post_json(
        &app,
        "/api/workspaces/select",
        serde_json::json!({ "path": repo1.path().to_string_lossy() }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    let _ = state.scheduler.cancel_run(&running.id).await;
}
