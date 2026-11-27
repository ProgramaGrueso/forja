# Entrega P8B — Refactorización de `crates/forja-server/src/orchestrator.rs`

## Resumen

Se dividió `crates/forja-server/src/orchestrator.rs` (en el árbol actual **4199 líneas**; originalmente referenciado con 3762 líneas antes de los cambios de persistencia/reacciones) en un módulo `crates/forja-server/src/orchestrator/` con submódulos por responsabilidad y un subdirectorio `tests/`. `mod.rs` queda en **194 líneas** (< 400). Se mantuvo intacta la API pública mediante `pub use`, sin cambiar lógica, textos ni JSON, sin añadir dependencias y sin tocar otros crates, `Cargo.toml` ni `docs/ROADMAP.md`.

## Estructura nueva (`crates/forja-server/src/orchestrator/`)

| Archivo | Líneas | Responsabilidad |
|---|---:|---|
| `mod.rs` | 194 | Declaración de submódulos, reexports públicos (`pub use`), definición de `Orchestrator` y métodos de ciclo de vida/configuración (`new`, `with_rules`, `from_factory`, `from_env`, `rules`, `agent`, `agents_available`, `agent_source`, `profiles`, `check_settings`, `apply_settings`, `apply_profile`, `profile`). |
| `backends.rs` | 533 | Constantes de entorno/orden (`PROFILE_ENV`, `AGENT_ENV`, `ORCHESTRATOR_AGENTS`, `AUTO_ORDER`), trait `ChatBackend`, `ChatReply`, `ChatOpts`, `ChatFuture`, `HermesBackend`, `CliBackend`, `BackendFactory`, resolución de perfil/agente (`resolve_profile`, `resolve_agent`, `agent_installed`, `installed_agents`, `AgentChoice`) y helpers de argv/sesión (`build_argv`, `build_argv_with`, `parse_session_id`). |
| `conversations.rs` | 683 | Estado en memoria de conversaciones (`Conversation`, `Inner`, `InFlight`, `InFlightGuard`), tipos públicos (`Message`, `ConversationSummary`, `ConversationDetail`, `ChatError`, `CreateError`), `history_block`, `auto_title`, persistencia (`with_persistence`, `flush_persistence`, `persist`), CRUD/reset/opciones en `Orchestrator` y handlers HTTP (`reset`, `list_conversations`, `create_conversation`, `get_conversation`, `delete_conversation`, `ResetRequest`, `CreateRequest`). |
| `delegation.rs` | 232 | Constantes (`MAX_AUTO_TURNS`, `AUTO_IDLE_WAIT`), estado `Reactions` y métodos de seguimiento de delegaciones, reacciones automáticas, límite de turnos automáticos y espera (`track_delegation`, `note_run_finished`, `try_schedule_reaction`, `take_reaction_batch`, `requeue_reaction`, `release_reaction`, `has_pending_reaction`, `tracked_runs`, `forget_pending_tasks`, `begin_auto_turn`, `abort_auto_turn`, `defer_reaction`, `wait_idle`). |
| `commands.rs` | 194 | Comandos de barra locales: `Command`, `COMMAND_HELP`, `help_text`, `parse_command`, `run_command` y `option_command`. |
| `chat.rs` | 397 | `system_prompt`, `Trigger`, flujo de turnos en `Orchestrator` (`say`, `say_in`, `say_in_with`, `auto_review`, `turn`) y handler HTTP `/api/orchestrator/chat` (`ChatRequest`, `chat`). |
| `tests/mod.rs` | 132 | Dobles y helpers compartidos de prueba (`Fake`, `Profiled`, `SessionFake`, `BarrierFake`, `HangFake`, `fake_rules`, `fake_script`, `base_settings`, `only`). |
| `tests/backends.rs` | 257 | Pruebas de perfiles, resolución de agentes, `HermesBackend`, `CliBackend`, `apply_settings`, `parse_session_id` y `build_argv` (12 pruebas). |
| `tests/commands.rs` | 45 | Pruebas de parseo de comandos de barra y texto de `/help` (2 pruebas). |
| `tests/conversations.rs` | 188 | Pruebas de `history_block`, sesiones aisladas, concurrencia entre conversaciones, cancelación con `/stop`, borrado, límites y títulos (8 pruebas). |
| `tests/delegation_flow.rs` | 955 | Pruebas de reacciones automáticas, bloques `[[forja:...]]`, opciones por conversación (`modelo`/`esfuerzo`) y comandos `/model`/`/effort` vía HTTP (22 pruebas). |
| `tests/http.rs` | 211 | Pruebas HTTP de `/chat`, `/reset`, CRUD de `/conversations` y comandos de barra (4 pruebas). |
| `tests/persistence.rs` | 302 | Pruebas de persistencia en disco, recarga tras reinicio, archivo corrupto, límites al cargar, exclusión de estado transitorio y tolerancia a fallos de escritura (6 pruebas). |
| **Total** | **4323** | *(Antes: 4199 líneas en `orchestrator.rs`; +124 líneas por cabeceras de módulo e imports)* |

## Pruebas antes y después

- **`orchestrator::tests` antes**: 54 pruebas (54 passed, 0 failed).
- **`orchestrator::tests` después**: 54 pruebas (54 passed, 0 failed):
  - `orchestrator::tests::backends`: 12
  - `orchestrator::tests::commands`: 2
  - `orchestrator::tests::conversations`: 8
  - `orchestrator::tests::delegation_flow`: 22
  - `orchestrator::tests::http`: 4
  - `orchestrator::tests::persistence`: 6
- **`forja-server` antes y después**: 149 pruebas (149 passed, 0 failed).
- **Workspace completo (`./scripts/check.sh`)**: **EXIT=0**
  - `cargo fmt --all --check`: limpio.
  - `cargo clippy --workspace --all-targets -- -D warnings`: limpio.
  - `cargo test --workspace`: **342 passed, 0 failed, 1 ignored** (`forja-agent`: 92 + 1 ignored; `forja-cli`: 45 lib + 9 integración [`app_cli`: 2, `e2e`: 1, `init_cli`: 2, `skills_cli`: 4]; `forja-core`: 14; `forja-git`: 11; `forja-sched`: 18; `forja-server`: 149; `forja-store`: 4).

## Criterios verificados

- [x] `orchestrator/` dividido por responsabilidad (`backends`, `conversations`, `delegation`, `commands`, `chat`, `mod.rs`).
- [x] API pública del módulo intacta mediante `pub use` (ningún otro archivo de `forja-server` o `forja-cli` requirió cambios).
- [x] Sin cambios en lógica, textos de respuesta ni formatos JSON.
- [x] Todas las 54 pruebas movidas a `orchestrator/tests/`.
- [x] `mod.rs` por debajo de 400 líneas (194 líneas).
- [x] Sin dependencias nuevas ni cambios en `docs/ROADMAP.md`.
- [x] `./scripts/check.sh` ejecutado hasta `EXIT=0`.

## Límites y decisiones

- **Conteo de líneas inicial**: el archivo `orchestrator.rs` en este worktree tenía **4199 líneas** (en lugar de las 3762 mencionadas en el encargo, debido a la persistencia de conversaciones incorporada previamente en la rama). Todas sus líneas y pruebas se migraron íntegras.
- **Encapsulación de `Orchestrator`**: los campos `inner`, `reactions` y `persistence` y el helper `backend()` se mantuvieron privados en `orchestrator/mod.rs` (visibles a todos los submódulos hijos en Rust sin exponer `Inner` ni `Reactions` a `pub(crate)`), evitando advertencias `private-interfaces`.
- **Handlers HTTP junto a su dominio**: los handlers CRUD y `/reset` quedaron en `conversations.rs` y `/chat` en `chat.rs`, reexportados desde `mod.rs` para mantener el enrutador de `lib.rs` sin cambios.
- **Colisión de nombre `delegation`**: `orchestrator/delegation.rs` convive sin ambigüedad con `crate::delegation` (el parser/ejecutor de bloques `[[forja:...]]`), referenciando explícitamente `crate::delegation` donde corresponde.
- **Archivo `tests/delegation_flow.rs` (955 líneas)**: se conservó cohesionado porque comparte `setup()`, `reaction_setup()`, `ReactionFake` y `wait_calls` entre las pruebas de reacciones automáticas, bloques de delegación y opciones por conversación.
