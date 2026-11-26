# PROGRESS — bitácora de Forja

> Léelo ENTERO al empezar y tras cualquier pérdida de contexto. Escribe aquí antes de pasar a la siguiente tarea.

## Estado actual
- Rama de integración: noche/forja
- Última tarea cerrada: T03 — forja-core: política de privacidad
- Tarea en curso: T04 (forja-git) y T06 (forja-agent) en paralelo vía subagentes
- Inicio: 2026-11-25T23:57:41-05:00
- Hora límite de trabajo: 2026-11-26T04:57:41-05:00

## Registro por tarea
### T01 — Workspace y scripts
- Estado: terminada
- Hora de inicio / fin: 2026-11-25T23:57:41-05:00 / 2026-11-25T23:58:31-05:00
- Quién la hizo: agente principal
- Commits: 5fbde52 T01: workspace con 7 crates y scripts/check.sh
- Comando de verificación y resultado real (pegar las últimas líneas de la salida):
  `./scripts/check.sh && cargo build --workspace` -> exit 0; 7/7 crates pasaron `fmt`, `clippy -D warnings`, `test` (7 tests ok) y `cargo build --workspace` (`Finished dev profile ... in 0.05s`).
- Intentos fallidos (máx. 3): ninguno.
- Notas para retomar: workspace listo.

### T02 — forja-core: tipos y errores
- Estado: terminada
- Hora de inicio / fin: 2026-11-25T23:58:35-05:00 / 2026-11-25T23:59:42-05:00
- Quién la hizo: agente principal
- Commits: 77b85ae T02: tipos de dominio y ForjaError en forja-core
- Comando de verificación y resultado real (pegar las últimas líneas de la salida):
  `cargo test -p forja-core && ./scripts/check.sh` -> exit 0; `forja-core`: `test result: ok. 7 passed; 0 failed; 0 ignored`; workspace completo en verde.
- Intentos fallidos (máx. 3): ninguno (solo `cargo fmt` inicial).
- Notas para retomar: Task, Privacy, AgentKind, RunStatus, RunRecord y ForjaError definidos y exportados.

### T03 — forja-core: política de privacidad
- Estado: terminada
- Hora de inicio / fin: 2026-11-25T23:59:45-05:00 / 2026-11-26T00:00:16-05:00
- Quién la hizo: agente principal
- Commits: (en commit T03)
- Comando de verificación y resultado real (pegar las últimas líneas de la salida):
  `cargo test -p forja-core policy && ./scripts/check.sh` -> exit 0; `policy`: `test result: ok. 4 passed; 0 failed; 0 ignored`; `forja-core`: `11 passed`; workspace en verde.
- Intentos fallidos (máx. 3): ninguno.
- Notas para retomar: Fase 0 cerrada. Arranca Fase 1 delegando T04 y T06 en paralelo.

### T04 — forja-git: crear worktree
- Estado: en curso
- Hora de inicio / fin: 2026-11-26T00:00:30-05:00 /
- Quién la hizo: subagente noche/forja-T04
- Commits:
- Comando de verificación y resultado real (pegar las últimas líneas de la salida):
- Intentos fallidos (máx. 3):
- Notas para retomar: worktree ../forja-wt-T04.

### T06 — forja-agent: trait Agent y MockAgent
- Estado: en curso
- Hora de inicio / fin: 2026-11-26T00:00:30-05:00 /
- Quién la hizo: subagente noche/forja-T06
- Commits:
- Comando de verificación y resultado real (pegar las últimas líneas de la salida):
- Intentos fallidos (máx. 3):
- Notas para retomar: worktree ../forja-wt-T06.

## Bloqueos (tarea, causa, qué se intentó)
- (ninguno)

## Dependencias añadidas fuera de la lista permitida
- (ninguna)

## Decisiones tomadas por mi cuenta (para que Juan las revise)
- Modelo para subagentes: `gemini-4-neon-low` para tareas acotadas/mecánicas, `gemini-4-neon-medium` para concurrencia/integración, `gemini-4-neon-high` para reintentos complejos.
- `--print-timeout`: se usa sufijo `s` (p. ej. `1800s`), confirmado por Juan y acorde con `agy --help` (`default 0s`).
- `PROMPT_INICIAL.md`: eliminado por indicación explícita de Juan.
- Rust edition `2021` en el workspace para máxima estabilidad entre subagentes; `Cargo.lock` versionado al incluir binario (`forja-cli`).

## Subagentes lanzados
| Hora | Tarea | Rama | Modelo | Resultado (aceptado / rechazado y por qué) |
|------|-------|------|--------|--------------------------------------------|

## Siguiente paso exacto
Crear la estructura del workspace de T01 y verificar con `./scripts/check.sh` y `cargo build --workspace`.
