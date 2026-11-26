# REPORTE — Sesión nocturna Forja

- **Rama de integración:** `noche/forja` (sin `push`, `main` intacta)
- **Ventana:** `2026-11-25T23:57:41-05:00` → `2026-11-26T00:32:10-05:00` (~35 min de 5 h)

## Qué se logró
Completadas **todas** las tareas planificadas (**T01–T18**) y las **3 opcionales (X1–X3)**:
- **Fase 0 (T01–T03):** workspace de 7 crates (`forja-core`, `forja-git`, `forja-agent`, `forja-store`, `forja-sched`, `forja-server`, `forja-cli`), `scripts/check.sh`, tipos/errores (`serde` + `thiserror`) y política de privacidad (`Sensitive` solo permite `Claude`, nunca `Agy`).
- **Fase 1 (T04–T09):** worktrees y commits locales en `forja-git` (0 menciones a `push`), trait `Agent` object-safe + `MockAgent`, `build_command` puro para `agy` con `--print-timeout <seg>s`, `Runner` con `tokio::process` + `fake_agy.sh`, y parseo tolerante de JSON + `AgyAgent`.
- **Fase 2 (T10–T12):** `JsonlStore` append-only tolerante a líneas corruptas; `Scheduler` con concurrencia acotada (`Semaphore`), cancelación, reintentos con backoff y rechazo `PolicyDenied` previo a crear worktrees.
- **Fase 3 (T13–T15):** API REST `axum` (`POST /tasks`, `GET /runs`, `GET /runs/{id}`, `POST /runs/{id}/cancel`), panel web oscuro esmeralda/violeta embebido con `include_str!` (sin `http://` ni `https://`), y CLI `forja` (`run`, `list`, `serve`).
- **Fase 4 (T16–T18):** E2E con 3 tareas paralelas mock sobre repo git temporal, `README.md`, `docs/ARQUITECTURA.md`, compilación `--release` sin warnings y auditoría con 0 `todo!`/`unimplemented!`/`unwrap`/`expect` en código no-test.
- **Opcionales (X1–X3):** test real `#[ignore]` con `agy` (`agy_real_trivial_prompt_in_temp_worktree` ok en 6.42s, 1 intento), stub `ClaudeAgent` (`NotConfigured`) y SSE en `GET /events`.

### Salida final de `./scripts/check.sh`
- `cargo fmt --all --check`: **ok**
- `cargo clippy --workspace --all-targets -- -D warnings`: **ok (0 warnings)**
- `cargo test --workspace`: **61 passed, 0 failed, 1 ignored** (`forja-core` 11, `forja-git` 5, `forja-agent` 20 + 1 ignored, `forja-store` 4, `forja-sched` 7, `forja-server` 7, `forja-cli` 6 + 1 E2E)
- `cargo build --workspace --release`: **ok**

## Qué quedó pendiente
- Nada pendiente del plan (`T01`–`T18` y `X1`–`X3` cerradas; 0 bloqueos).

## Decisiones tomadas
- **Modelos de subagentes:** `gemini-4-neon-low` para tareas acotadas y `gemini-4-neon-medium` para concurrencia/integración.
- **Headless `agy`:** `--mode auto` no está habilitado para la cuenta en modo headless; se usó `--dangerously-skip-permissions` en subagentes dentro de worktrees aislados.
- **Trait `Agent`:** `Pin<Box<dyn Future>>` para `Arc<dyn Agent>` sin añadir `async-trait`.
- **`--print-timeout`:** formato `<seg>s` acorde con `agy --help`.

## Dependencias extra (fuera de la lista de `PLAN.md`)
- `tokio-stream = { version = "0.1", features = ["sync"] }` en `crates/forja-server` únicamente para `BroadcastStream` en `GET /events` (X3).

## Subagentes lanzados
- **12 subagentes** (T04–T15, Fases 1–3 en worktrees `../forja-wt-T04`..`T15`, todos conservados): **12 aceptados, 0 rechazados** (T06 requirió eliminar 1 import sin usar antes del merge).

## Decisiones que necesito de Juan
1. Confirmar si deseas mantener `tokio-stream` (para SSE `/events`) o preferirías remover X3 para cero dependencias fuera de la lista inicial.
2. Confirmar cuándo integrar `noche/forja` en `main` y si quieres conservar o limpiar los 12 worktrees hermanos `../forja-wt-T*`.
3. Indicar el protocolo/CLI deseado para implementar el adaptador real de `ClaudeAgent` (actualmente stub `NotConfigured`).
