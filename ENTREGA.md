# ENTREGA — Forja

## Resumen
Se implementó el workspace Rust modular **Forja** en la rama `noche/forja` cubriendo las 18 tareas principales (`T01`–`T18`) y las 3 opcionales (`X1`–`X3`) de `PLAN.md`. Detalle ejecutivo en [REPORTE.md](REPORTE.md) y bitácora completa en [PROGRESS.md](PROGRESS.md).

## Criterios cumplidos
- [x] Workspace de 7 crates con `scripts/check.sh` en verde (`fmt`, `clippy -D warnings`, `test --workspace`).
- [x] Política de privacidad: `Sensitive` solo permite `Claude` y rechaza `Agy` con `ForjaError::PolicyDenied` antes de crear worktrees.
- [x] Aislamiento git en `forja-git` sin ninguna función ni referencia a `push`.
- [x] Agentes `MockAgent`, `AgyAgent` (con `fake_agy.sh` en tests normales y test `#[ignore]` real validado en 1 intento) y `ClaudeAgent` stub (`NotConfigured`).
- [x] Persistencia `JsonlStore` append-only tolerante a líneas corruptas; `Scheduler` con concurrencia acotada, reintentos, cancelación y difusión de eventos.
- [x] API REST `axum`, SSE en `GET /events`, panel web autocontenido sin URLs externas (`http://`/`https://`) y CLI `forja` (`run`, `list`, `serve`).
- [x] E2E de 3 tareas paralelas con worktrees, ramas y commits reales, documentación (`README.md`, `docs/ARQUITECTURA.md`) y compilación `--release` limpia.

## Criterios no cumplidos
- Ninguno.

## Evidencia de verificación (salidas reales)
- `./scripts/check.sh`: exit `0` — `cargo fmt --all --check` ok, `cargo clippy --workspace --all-targets -- -D warnings` ok, `cargo test --workspace` **61 passed; 0 failed; 1 ignored**.
- `cargo test -p forja-agent -- --ignored agy_real`: exit `0` — `test agy::tests::agy_real_trivial_prompt_in_temp_worktree ... ok` (6.42s).
- `cargo test -p forja-cli --test e2e`: exit `0` — `test e2e_three_parallel_mock_tasks_create_worktrees_branches_commits_and_store ... ok` (0.08s).
- `cargo run -p forja-cli -- list`: exit `0` — `No hay ejecuciones registradas.`
- `cargo build --workspace --release`: exit `0` — `Finished release profile [optimized] target(s)`.
- `grep -rn "todo!\|unimplemented!" crates`: sin coincidencias.
- `grep -rn "push" crates/forja-git/src`: sin coincidencias.

## Dudas abiertas / puntos para revisión
- Se añadió `tokio-stream` en `forja-server` exclusivamente para X3 (`GET /events` SSE); revisar si se conserva o se prefiere evitar dependencias fuera de la lista base.
- `panel.html` utiliza polling cada 3s a `/runs`; opcionalmente puede conectarse en el futuro a `/events` (SSE) desde el frontend.
- Sugerido pasar revisión cruzada en Claude Code u opencode antes de integrar `noche/forja` en `main`.
