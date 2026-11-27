# Entrega P8D — Refactorización de `crates/forja-sched/src/lib.rs`

## Resumen

Se dividió `crates/forja-sched/src/lib.rs` (originalmente **1384 líneas**) en módulos internos con responsabilidades acotadas, dejando `lib.rs` en **307 líneas** sin modificar la API pública, los tipos exportados, los reexports ni el comportamiento observable. No se tocaron otros crates, `crates/forja-sched/src/output.rs`, `Cargo.toml` ni `docs/ROADMAP.md`.

## Estructura nueva (`crates/forja-sched/src/`)

| Archivo | Líneas | Responsabilidad |
|---|---:|---|
| `lib.rs` | 307 | Declaración de módulos, reexports públicos (`SchedulerConfig`, `output::*`), `SchedulerInner`, API pública de `Scheduler` (encolado, espera, consulta, cancelación, suscripciones de eventos/salida) y `now_ms`. |
| `config.rs` | 27 | `SchedulerConfig` y su constructor `SchedulerConfig::new`. |
| `concurrency.rs` | 62 | Control de concurrencia dinámico: `ConcurrencyState`, `ConcurrencyLimiter` y `ConcurrencyPermit` (`Drop`). |
| `buffers.rs` | 52 | Gestión en memoria de buffers de salida retenidos (`MAX_RETAINED_OUTPUTS`, `OutputBuffers::get`/`register` con descarte de terminados) y helper `push_lines`. |
| `worktree.rs` | 48 | Preparación de worktrees git o directorios de trabajo (`prepare_worktree`) y commit automático al finalizar (`commit_worktree_changes`). |
| `runner.rs` | 194 | Ejecución de tareas (`run_task`), bucle de reintentos (`execute_with_retries`), streaming/fallback de salida y limpieza del campo `effort` ante `EFFORT_NOT_APPLIED_WARNING`. |
| `output.rs` | 181 | *(Sin cambios)* Buffer de salida en vivo (`RunOutput`, `OutputChunk`, `OutputPage`, `OutputSubscription`, `is_valid_run_id`). |
| `tests/mod.rs` | 202 | Helpers y agentes de prueba compartidos (`CountingAgent`, `FlakyAgent`, `ScriptAgent`, `run_git`, `init_temp_repo`, `new_scheduler`, `open_task`). |
| `tests/concurrency.rs` | 155 | Pruebas de límite de concurrencia, cancelación y actualización dinámica del límite (3 pruebas). |
| `tests/execution.rs` | 220 | Pruebas de worktrees git, persistencia de errores, política de privacidad, reintentos y fallback de `effort` (6 pruebas). |
| `tests/streaming.rs` | 205 | Pruebas de salida en vivo, topes por chunks/bytes, paralelismo, replay, redacción de secretos e IDs inválidos (9 pruebas). |
| **Total** | **1653** | *(Antes: 1565 líneas = 1384 `lib.rs` + 181 `output.rs`)* |

## Mapa de qué quedó en cada módulo

- **`lib.rs`**:
  - `pub mod output;` y `pub use output::{is_valid_run_id, OutputChunk, OutputPage, OutputSubscription};`
  - `pub use config::SchedulerConfig;`
  - `SchedulerInner` (`pub(crate)`) y `pub struct Scheduler` con todos sus métodos públicos intactos (`new`, `max_concurrency`, `repo_path`, `base_branch`, `worktrees_dir`, `set_max_concurrency`, `subscribe`, `set_output_limits`, `output_since`, `subscribe_output`, `submit`, `wait_run`, `wait_all`, `list_runs`, `get_run`, `cancel_run`) y el helper privado `output_for`.
  - `now_ms()` (`pub(crate)`).
- **`config.rs`**: `SchedulerConfig` y `SchedulerConfig::new`.
- **`concurrency.rs`**: `ConcurrencyState`, `ConcurrencyLimiter` (`new`, `acquire`, `set_limit`, `limit`) y `ConcurrencyPermit` (`Drop`).
- **`buffers.rs`**: `MAX_RETAINED_OUTPUTS`, `OutputBuffersState`, `OutputBuffers` (`get`, `register`) y `push_lines`.
- **`worktree.rs`**: `prepare_worktree` y `commit_worktree_changes` (reciben parámetros explícitos para desacoplarse de `SchedulerInner`).
- **`runner.rs`**: `run_task` y `execute_with_retries`.
- **`tests/`**:
  - `tests/concurrency.rs`: `concurrency_limits_parallel_runs_to_n`, `concurrency_cancel_run_marks_cancelled`, `concurrency_dynamic_limit_updates_live`.
  - `tests/execution.rs`: `scheduler_creates_git_worktree_and_commits_changes`, `scheduler_persists_failed_run_error`, `policy_denies_sensitive_with_agy_without_creating_worktree`, `retry_succeeds_on_third_attempt`, `opencode_variant_fallback_is_not_a_failed_attempt_and_clears_effort`, `retry_exhausted_marks_failed`.
  - `tests/streaming.rs`: `output_streams_live_before_run_finishes`, `output_buffer_is_capped_and_since_reconnects`, `output_byte_cap_drops_oldest`, `two_runs_stream_in_parallel_without_mixing`, `max_concurrency_still_applies_to_streaming_runs`, `finished_run_replays_buffer_and_closes`, `non_streaming_agent_emits_output_at_end`, `output_is_redacted_before_emission`, `output_unknown_or_invalid_id_is_not_found`.

## Pruebas antes y después

- **`forja-sched` antes**: 18 pruebas unitarias (18 passed, 0 failed).
- **`forja-sched` después**: 18 pruebas unitarias (18 passed, 0 failed; 3 en `tests::concurrency`, 6 en `tests::execution`, 9 en `tests::streaming`).
- **Workspace completo (`./scripts/check.sh`)**: **EXIT=0**
  - `cargo fmt --all --check`: limpio.
  - `cargo clippy --workspace --all-targets -- -D warnings`: limpio.
  - `cargo test --workspace`: **332 passed, 0 failed, 1 ignored** (`forja-agent`: 92 + 1 ignored; `forja-cli`: 45 lib + 9 integración; `forja-core`: 14; `forja-git`: 11; `forja-sched`: 18; `forja-server`: 139; `forja-store`: 4).

## Límites y decisiones

- **Persistencia (`JsonlStore` y `store_lock`)**: no se extrajo un módulo de persistencia separado porque las operaciones sobre `store`, `runs` y `events` comparten exactamente la misma sección crítica bajo `store_lock` y `runs.write()` en `Scheduler::{submit, cancel_run}` y `runner::{run_task, execute_with_retries}`. Separarlo habría requerido abstraer callbacks/closures alrededor de locks async con riesgo de alterar el orden de adquisición o la visibilidad de eventos sin ganancia real de claridad.
- **API pública y otros crates**: ningún otro crate requirió cambios ni ajustes de imports.
- **`output.rs`**: se mantuvo sin modificaciones por estar ya separado y fuera del alcance de `lib.rs`.
- **Entorno de ejecución de `check.sh`**: en el sandbox `/tmp` es de solo lectura, por lo que `./scripts/check.sh` se ejecutó con `TMPDIR` apuntando a un directorio temporal escribible fuera del repositorio para no interferir con pruebas de `forja-cli` que verifican detección de directorios fuera de git.
