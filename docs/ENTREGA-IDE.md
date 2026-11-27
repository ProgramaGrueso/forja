# Entrega — Modo IDE (selector de carpeta y vista Archivos)

## Qué se hizo

### Backend (`forja-sched` y `forja-server`)
- **Cambio de workspace en caliente (`crates/forja-sched/src/lib.rs`, `runner.rs`)**:
  - `repo_path` y `base_branch` en `SchedulerInner` pasan a estar protegidos por `std::sync::RwLock`.
  - Nuevos métodos `Scheduler::has_active_runs()` y `Scheduler::set_workspace(repo_path, base_branch)`, que rechazan el cambio si hay ejecuciones en estado `Pending` o `Running`.
- **Servicio y API de workspaces (`crates/forja-server/src/workspaces.rs`)**:
  - `WorkspaceService` mantiene la carpeta activa (`active_root`) y el historial de recientes (máx. 15) en `$XDG_CONFIG_HOME/forja/workspaces.json` (o `~/.config/forja/workspaces.json`), con `AppState::with_workspaces_path` para pruebas aisladas.
  - Endpoints bajo `security::guard` y `auth_middleware`:
    - `GET /api/workspaces`: carpeta activa (`path`, `name`, `is_git`, `branch`) y recientes.
    - `GET /api/workspaces/browse?path=&show_hidden=false`: navegador de subdirectorios locales.
    - `POST /api/workspaces/select`: valida y canonicaliza el directorio, detecta rama Git vía `forja_git::current_branch`, actualiza el `Scheduler` (responde `409 Conflict` si hay ejecuciones activas) y persiste recientes.
- **API segura de archivos (`crates/forja-server/src/files.rs`)**:
  - Validación por `canonicalize()` + `starts_with(canonical_root)` contra *path traversal* (`..`, rutas absolutas, bytes nulos), *symlinks* que escapen de la raíz y escritura en `.git`.
  - Soporte opcional de lectura sobre worktrees (`?run_id=`), validando que el worktree cuelgue de `worktrees_dir` y marcando `read_only = true`.
  - Endpoints bajo `security::guard` y `auth_middleware`:
    - `GET /api/files/tree?path=&run_id=`: listado ordenado (carpetas primero), omitiendo `.git`, `target`, `node_modules` y `.forja`.
    - `GET /api/files/read?path=&run_id=`: lectura UTF-8 hasta 512 KiB, rechazo de binarios (`400`) y detección de lenguaje.
    - `PUT /api/files/write`: escritura atómica (temporal + `rename`) solo sobre el workspace activo, con `DefaultBodyLimit::max(1024 * 1024)` en esa ruta sin cambiar `MAX_BODY_BYTES` global (256 KiB).
    - `GET /api/files/search?q=&run_id=`: búsqueda rápida acotada (profundidad máx. 8, máx. 60 resultados).

### Frontend (`crates/forja-server/build.rs` y `src/panel/`)
- **Selector de carpeta en la barra lateral (`#workspace-bar`) y modal navegador (`#workspace-modal`)**: muestra la carpeta y rama Git activas, lista de recientes, entrada de ruta y navegación por directorios locales.
- **Nueva vista «Archivos» (`#view-archivos`, `css/archivos.css`, `js/archivos.js`)**:
  - Layout IDE de dos columnas: árbol colapsable con caché, buscador rápido con debounce (`200 ms`) y conmutador «Proyecto base / Worktree de ejecución».
  - Múltiples pestañas de archivos abiertos (indicador de cambios sin guardar `●`, cierre con `×` y persistencia por workspace en `localStorage`).
  - Editor con gutter de números de línea sincronizado, `Tab` (2 espacios), guardado con `Ctrl+S` / `Cmd+S` y botón **«Citar en Orquestador»**.
- Ensamblado en compilación desde `build.rs` sin recursos externos (`http://`/`https://` = 0 coincidencias).

## Criterios de aceptación

| Criterio | Estado |
|---|---|
| Selector de carpeta en caliente con recientes y navegador local, bloqueando cambio con ejecuciones activas | Cumplido (`workspaces.rs`, `#workspace-bar`, `#workspace-modal`) |
| API segura de archivos (`/tree`, `/read`, `/write`, `/search`) con `canonicalize`, tope 512 KiB, rechazo de binarios y `?run_id=` | Cumplido (`files.rs`, `router.rs`) |
| Vista «Archivos» tipo IDE con árbol, pestañas, editor con `Ctrl+S`, números de línea y «Citar en Orquestador» | Cumplido (`index.html`, `archivos.css`, `archivos.js`) |
| Embebido en el binario sin URLs externas | Cumplido (`build.rs`, `panel_get_root_returns_html_without_external_urls`) |
| Pruebas automatizadas y `./scripts/check.sh` en verde | Cumplido |

## Verificación

```text
$ ./scripts/check.sh
EXIT=0
- cargo fmt --all --check: OK
- cargo clippy --workspace --all-targets -- -D warnings: OK
- forja-sched: 19 passed; 0 failed
- forja-server: 156 passed; 0 failed (incluye files_workspaces + panel_mascot)
- forja-store: 4 passed; 0 failed
- resto del workspace: todo en verde (1 ignored en forja-agent por binario agy real)
```

Pruebas nuevas añadidas:
- `tests::files_workspaces::files_tree_read_write_and_search`
- `tests::files_workspaces::files_security_rejects_traversal_symlinks_git_and_binary`
- `tests::files_workspaces::files_read_worktree_with_run_id_is_read_only`
- `tests::files_workspaces::workspaces_get_browse_select_and_conflict_on_active_run`
- Aserciones ampliadas en `tests::panel_mascot::panel_views_modal_filters_and_settings_present` y `panel_assembled_html_includes_sections_css_and_js_in_order`.

## Límites conocidos y decisiones

- Cambiar de carpeta actualiza el repositorio y la rama base del `Scheduler` para las nuevas ejecuciones y recarga el árbol/pestañas del IDE, pero mantiene el `store_dir` (`runs.jsonl`, `settings.json`, `conversations.json`) con el que se inició el servidor.
- Los worktrees (`?run_id=`) se abren en modo **solo lectura** para evitar interferir con ramas gestionadas por el ciclo de revisión (`diff`/`merge`/`discard`).
- El editor usa `<textarea>` nativo con gutter sincronizado para mantener cero dependencias externas; resaltado sintáctico por tokens puede añadirse sobre la misma arquitectura en una iteración siguiente.
