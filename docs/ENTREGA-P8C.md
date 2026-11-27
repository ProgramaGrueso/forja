# Entrega P8C — Refactorización de `crates/forja-server/src/lib.rs`

## Resumen

Se dividió `crates/forja-server/src/lib.rs` (originalmente **2071 líneas** en esta rama) en módulos internos por responsabilidad, dejando `lib.rs` como fachada de **46 líneas** con las declaraciones de módulos, `pub use`, `build_router` y `serve`. Se preservó exactamente la API pública, el comportamiento HTTP, los códigos de estado, los payloads JSON y los mensajes de error. No se tocaron `panel.html`, `orchestrator.rs`, `docs/ROADMAP.md`, `Cargo.toml` ni ningún otro crate del workspace.

## Estructura nueva (`crates/forja-server/src/`)

| Archivo | Líneas | Responsabilidad |
|---|---:|---|
| `lib.rs` | 46 | Fachada: declaraciones de módulos públicos e internos, `pub use` (`AppState`, `CreateTaskRequest`, `MAX_BODY_BYTES`, `mascot_bytes`), reexport interno `ApiError`, `build_router` y `serve`. |
| `state.rs` | 293 | `AppState` y su construcción/configuración (`new`, `with_*`, `without_agent`, `on_settings_changed`, `agent_for_task`, `submit_task` y resolución de modelo/esfuerzo). |
| `router.rs` | 104 | `MAX_BODY_BYTES`, ensamblado de rutas públicas y protegidas (`build_router_for_port`) con límite de cuerpo y guardas de seguridad, y `auth_middleware`. |
| `handlers/mod.rs` | 48 | Declaración y reexport de submódulos de handlers HTTP, `ErrorBody` y `ApiError` (`From<ForjaError>`, `IntoResponse`). |
| `handlers/tasks.rs` | 58 | `CreateTaskRequest`, defaults de privacidad/agente, `TaskQuery` y handler `POST /tasks` (`create_task`). |
| `handlers/runs.rs` | 30 | Handlers de ejecuciones: `GET /runs` (`list_runs`), `GET /runs/{id}` (`get_run`) y `POST /runs/{id}/cancel` (`cancel_run`). |
| `handlers/stream.rs` | 110 | SSE global `GET /events` (`events_handler`), `GET /runs/{id}/output` (`run_output`) y SSE por ejecución `GET /runs/{id}/stream` (`run_stream`) con sus DTOs. |
| `handlers/panel.rs` | 7 | Embebido de `panel.html` (`include_str!("../panel.html")`) y handler `GET /` (`panel_handler`). |
| `handlers/mascot.rs` | 46 | Sprites embebidos de la mascota (`MASCOT_FILES`), función pública `mascot_bytes` y handler `GET /mascot/{name}` (`mascot_handler`). |
| `tests/mod.rs` | 148 | Helpers compartidos de pruebas (`test_state`, `read_json`, `run_git`, `init_git_repo`, `test_state_with_repo`, `get`, `post`, `finished_task`, `wait_until_running`). |
| `tests/tasks_runs.rs` | 320 | Pruebas de alta de tareas, listado/consulta/cancelación de ejecuciones, validaciones, límite de cuerpo y `/api/models` (8 pruebas). |
| `tests/auth_security.rs` | 280 | Pruebas de autenticación Bearer/query token y guardas de `Host`, `Origin` y `Sec-Fetch-Site` (6 pruebas). |
| `tests/panel_mascot.rs` | 203 | Pruebas de HTML del panel (sin URLs externas, vistas, navegación, controles) y sprites de mascota (4 pruebas). |
| `tests/stream_output.rs` | 228 | Pruebas de SSE `/events`, salida paginada `/output?since=` y streaming `/stream` en vivo y finalizado (5 pruebas). |
| `tests/review.rs` | 270 | Pruebas del ciclo de revisión git (`/diff`, `/merge`, `/discard`, árbol sucio, fallos y cancelaciones) (7 pruebas). |
| **Total** | **2191** | *(Antes: 2071 líneas en un único `lib.rs`)* |

## Mapa de qué quedó en cada módulo

- **`lib.rs`**: conserva exactamente las mismas exportaciones públicas que antes (`pub mod api`, `conversation_store`, `delegation`, `models`, `orchestrator`, `quota`, `reactor`, `review`, `settings`, `telemetry`; `pub use` de `AppState`, `CreateTaskRequest`, `MAX_BODY_BYTES`, `mascot_bytes`; funciones `build_router` y `serve`) y `pub(crate) use handlers::ApiError` para `review.rs`.
- **`state.rs`**: único ajuste de visibilidad interno fue `pub(crate) watcher_started: Arc<AtomicBool>` en `AppState` para que `reactor.rs` pueda seguir accediéndolo sin modificar `reactor.rs`.
- **`handlers/panel.rs` y `handlers/mascot.rs`**: ajustaron sus rutas relativas en `include_str!("../panel.html")` e `include_bytes!("../../assets/mascot/*.webp")` respecto a `src/handlers/`.
- **`tests/`**:
  - `tests::tasks_runs` (8): `api_post_task_and_list_and_get_run`, `api_post_task_policy_denied_returns_403`, `api_cancel_run_marks_cancelled`, `api_get_missing_run_returns_404`, `api_post_task_unconfigured_agent_returns_400`, `api_post_task_rejects_empty_title_or_prompt`, `api_body_limit_rejects_oversized_payload`, `api_models_returns_catalog_from_injected_runner`.
  - `tests::auth_security` (6): `api_bearer_auth_protects_routes_when_enabled`, `security_rejects_foreign_and_malformed_hosts`, `security_accepts_all_local_host_forms`, `security_rejects_invalid_origin_on_mutating_and_read_routes`, `security_allows_valid_local_origin_and_cli_requests`, `security_rejects_cross_site_fetch_metadata`.
  - `tests::panel_mascot` (4): `panel_get_root_returns_html_without_external_urls`, `panel_contains_nav_agents_provider_bar_and_mascot_svg`, `mascot_images_are_served_without_auth_and_unknown_is_404`, `panel_views_modal_filters_and_settings_present`.
  - `tests::stream_output` (5): `events_sse_streams_run_updates`, `run_output_endpoint_supports_since`, `run_output_and_stream_unknown_id_404`, `run_stream_sse_live_then_status_and_close`, `run_stream_finished_run_replays_and_closes`.
  - `tests::review` (7): `review_diff_then_merge_closes_worktree_cycle`, `review_discard_removes_worktree_branch_and_changes`, `review_rejects_missing_run_and_server_without_repo`, `review_merge_rejects_active_run_and_dirty_base`, `review_merge_rejects_failed_run_but_discard_cleans_it`, `review_merge_commits_pending_worktree_changes`, `review_cancelled_run_can_be_discarded_but_not_merged`.

## Pruebas antes y después

- **`forja-server` antes**: 151 pruebas unitarias en `--lib` (incluyendo las 30 pruebas en `lib.rs::tests`).
- **`forja-server` después**: 151 pruebas unitarias en `--lib` (151 passed, 0 failed; las mismas 30 pruebas distribuidas en `tests::{tasks_runs, auth_security, panel_mascot, stream_output, review}` sin debilitar aserciones).
- **Verificación completa (`./scripts/check.sh`)**: **EXIT=0**
  - `cargo fmt --all --check`: limpio.
  - `cargo clippy --workspace --all-targets -- -D warnings`: limpio.
  - `cargo test --workspace`: **347 passed, 0 failed, 1 ignored** (`forja-agent`: 92 + 1 ignored; `forja-cli`: 46 lib + 9 integración; `forja-core`: 15; `forja-git`: 11; `forja-sched`: 19; `forja-server`: 151; `forja-store`: 4).

## Límites y decisiones

- **Ubicación de `tests/`**: las 30 pruebas se ubicaron en `crates/forja-server/src/tests/` (`#[cfg(test)] mod tests;`) siguiendo la misma convención que P8D (`crates/forja-sched/src/tests/`), manteniendo un solo binario de pruebas unitarias para `forja-server` y evitando duplicar compilación/enlazado en `crates/forja-server/tests/` (que actualmente almacena scripts y fixtures).
- **Archivos intocados**: `panel.html`, `orchestrator.rs`, `docs/ROADMAP.md`, `Cargo.toml` y el resto de módulos de `forja-server` (`api.rs`, `reactor.rs`, `review.rs`, `security.rs`, etc.) permanecieron sin cambios.
- **Dependencias**: no se añadió ni modificó ninguna dependencia.
