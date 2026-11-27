# Entrega P5: Persistencia de conversaciones del orquestador

## Qué se hizo

Se implementó la persistencia en disco de las conversaciones del orquestador en `<store_dir>/conversations.json` (junto a `runs.jsonl`), sin añadir dependencias nuevas y conservando la API HTTP y los límites actuales (`MAX_CONVERSATIONS = 20`, `MAX_MESSAGES = 500`).

### Archivos modificados / añadidos

- `crates/forja-sched/src/lib.rs`:
  - Nuevo método `Scheduler::store_dir(&self) -> PathBuf` para exponer el directorio real del store (`runs.jsonl`).
- `crates/forja-server/src/conversation_store.rs` (nuevo):
  - `PersistedConversation`: serializa exactamente `id`, `titulo`, `session_id`, `historial` (`rol`, `texto`, `ts`), `creado`, `actualizado`, `modelo` y `esfuerzo`. Excluye explícitamente `in_flight`, `pending` y `auto_turns`.
  - `enforce_limits`: descarta IDs vacíos, resuelve duplicados quedándose con el `actualizado` más reciente, conserva siempre `principal` si está presente, acota el total a `MAX_CONVERSATIONS` (descartando las más antiguas por `actualizado`, reservando hueco para `principal`) y recorta cada historial a los `MAX_MESSAGES` mensajes más recientes.
  - `load_conversations`: si el archivo no existe devuelve vacío sin avisos; si falla la lectura, el JSON está corrupto o faltan campos raíz, emite un único aviso por `stderr` y devuelve vacío; si solo algunas entradas del array son inválidas, ignora únicamente esas entradas y emite un único aviso con el conteo. Nunca impide el arranque.
  - `write_atomic`: aplica `enforce_limits`, escribe JSON en un archivo temporal en el mismo directorio (`.conversations.json.tmp.<pid>.<seq>`), ejecuta `sync_all()` y realiza `fs::rename` atómico (limpiando el temporal en caso de error).
  - `ConversationPersistence`: escritor en hilo de fondo dedicado (`forja-conv-store`) con coalescencia de snapshots (`schedule` no bloquea el turno de chat ni retiene locks de `Orchestrator`), `flush()` para sincronización en tests y volcado ordenado en `Drop`. Si falla la escritura, registra aviso por `stderr` sin propagar el error al usuario; tras 2 fallos consecutivos detiene los reintentos hasta el siguiente cambio.
- `crates/forja-server/src/orchestrator.rs`:
  - `Orchestrator::with_persistence(path)` carga el estado válido al arrancar, asegura que `principal` exista siempre, actualiza el contador interno `counter` con el mayor sufijo `c<N>-...` cargado para evitar colisiones de IDs tras reinicios e inicializa `in_flight = None`, `pending = Vec::new()`, `auto_turns = 0`.
  - `Orchestrator::persist()` toma un snapshot fuera del lock y programa el guardado coalescido al terminar cada turno de chat (`turn`), al crear (`create`), borrar (`delete`), renombrar (`rename`), reiniciar (`reset_conversation`), cambiar modelo o esfuerzo (`update_options`), limpiar opciones/sesiones al cambiar de agente o perfil (`clear_conversation_options`, `reset_all_sessions`) y al añadir mensajes del sistema (`push_system`, `defer_reaction`). No guarda por token.
- `crates/forja-server/src/lib.rs`:
  - Exporta `pub mod conversation_store;`.
  - `AppState::new` construye el orquestador con `.with_persistence(scheduler.store_dir().join(conversation_store::CONVERSATIONS_FILE))`.

## Criterios de aceptación

| Criterio | Estado |
| --- | --- |
| 1. Persistencia atómica en `<store_dir>/conversations.json` sin bloquear el turno | Cumplido |
| 2. Campos persistidos exactos; sin `in_flight`, `pending` ni `auto_turns` | Cumplido |
| 3. Carga al arrancar tolerante a ausencia o corrupción con un solo aviso | Cumplido |
| 4. `principal` siempre presente y límites 20/500 aplicados al cargar y guardar | Cumplido |
| 5. Guardado coalescido en fin de turno, crear/borrar/renombrar y cambio de modelo/esfuerzo | Cumplido |
| 6. Compatibilidad total con `/api/orchestrator/conversations`, `/chat`, `/reset` y conservación de IDs | Cumplido |
| 7. Fallos de guardado no se propagan al usuario; corte tras 2 fallos seguidos hasta el siguiente cambio | Cumplido |

## Pruebas añadidas

En `crates/forja-server/src/conversation_store.rs`:
- `conversation_store::tests::missing_file_loads_empty`
- `conversation_store::tests::partial_corruption_ignores_only_bad_entries`
- `conversation_store::tests::enforce_limits_keeps_principal_and_newest_20_and_500_msgs`
- `conversation_store::tests::stops_retrying_after_two_consecutive_failures_until_next_change`

En `crates/forja-server/src/orchestrator.rs` (`orchestrator::tests::persistence_tests`):
- `persistence_roundtrip_two_conversations_and_messages`: ida y vuelta con dos conversaciones, sus mensajes, `session_id`, `modelo`, `esfuerzo` y renombrado.
- `persistence_reload_in_new_instance_simulates_restart`: recarga en una instancia nueva simulando reinicio, reanudación de `session_id` y creación sin colisión de IDs.
- `persistence_corrupt_file_starts_with_principal_only`: archivo corrupto arranca solo con `principal` y sigue operativo regenerando JSON válido.
- `persistence_enforces_20_conversations_and_500_messages_on_load`: descarta las conversaciones más antiguas por `actualizado` (conservando `principal`) y recorta los historiales más viejos a 500 mensajes.
- `persistence_excludes_in_flight_pending_and_auto_turns`: verifica que `in_flight`, `pending` y `auto_turns` no se escriben al JSON y quedan en `None`/vacío/`0` al recargar.
- `persistence_write_failure_does_not_fail_user_turn`: un error de escritura en disco no falla las operaciones ni el turno del usuario.

## Verificación

Comando ejecutado: `./scripts/check.sh` (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`).

- Resultado: `EXIT=0`
- `forja-agent`: 92 passed, 0 failed, 1 ignored (`agy_real_trivial_prompt_in_temp_worktree`)
- `forja-cli`: 54 passed (45 unit + 9 integración), 0 failed
- `forja-core`: 14 passed, 0 failed
- `forja-git`: 11 passed, 0 failed
- `forja-sched`: 18 passed, 0 failed
- `forja-server`: 149 passed, 0 failed (incluyendo las 10 pruebas nuevas de persistencia)
- `forja-store`: 4 passed, 0 failed

## Límites conocidos

- Si el proceso recibe `SIGKILL` justo después de terminar un turno pero antes de que el hilo de fondo complete el `rename`, puede perderse el último cambio en vuelo (trade-off intencionado para no bloquear el turno de chat). En apagado normal (`Drop`), el hilo completa el volcado pendiente antes de salir.
- Al cargar un archivo corrupto parcialmente a nivel de entrada, cualquier conversación con un mensaje malformado dentro de `historial` se descarta completa como entrada inválida (no se repara mensaje a mensaje).
- Si se cambia `store_dir` únicamente en caliente en `settings.json` sin reiniciar el servidor, el `Scheduler` y la persistencia siguen apuntando al `store_dir` con el que arrancó el proceso hasta el próximo reinicio.
