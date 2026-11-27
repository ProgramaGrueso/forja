# Entrega — punto 7: reacción automática al terminar tareas delegadas

## Qué se hizo

- **`reactor` (nuevo módulo del servidor).** Se suscribe al `broadcast` del
  planificador y se queda con los estados finales (`done`, `failed`, `cancelled`)
  de las ejecuciones que el orquestador delegó.
- **Registro de delegaciones.**
  - Orquestadores sin sesión (claude, opencode, agy, local): al ejecutar sus
    bloques `[[forja:delegar ...]]` se registra cada `run_id` con la conversación.
  - Hermes: su `curl` a `/tasks` añade `?conversation_id=<id>`; el servidor
    registra igual la ejecución. El prompt del sistema incluye el parámetro y
    pide no quitarlo.
- **Turno automático.** Al terminar, el orquestador recibe un aviso con ID,
  título, agente, estado, salida y error resumidos (redactados y recortados),
  sin esperar a que el usuario escriba. Después puede revisar y seguir
  delegando.
- **Sin duplicados ni carreras.**
  - El aviso se consume al primer `note_run_finished`; el evento SSE y la
    reconciliación posterior no disparan dos veces.
  - Si la tarea terminó antes de registrarse, una reconciliación comprueba el
    estado actual.
  - Solo hay una reacción en curso por conversación; los fines de tarea que
    llegan mientras tanto se agrupan en el siguiente lote.
  - El turno automático espera a que la conversación esté libre. Si el usuario
    escribe durante uno en vuelo, su mensaje cancela el automático y continúa.
- **Sin bucles infinitos.** `MAX_AUTO_TURNS = 8` turnos automáticos seguidos por
  mensaje del usuario. Al agotarse, los resultados se dejan como «Tareas
  delegadas» para el siguiente mensaje; un mensaje del usuario reinicia el
  contador.
- **Pruebas añadidas** en `crates/forja-server/src/orchestrator.rs` y
  `crates/forja-server/src/reactor.rs`:
  - fin de delegación que despierta al orquestador sin mensaje del usuario;
  - tarea terminada antes de registrar el seguimiento (reconciliación sin
    duplicados);
  - `POST /tasks?conversation_id=...` (camino Hermes) que dispara el aviso;
  - tope de turnos automáticos y resultados pendientes para el usuario;
  - espera cuando el usuario tiene un turno en curso;
  - interrupción de un turno automático en vuelo por un mensaje del usuario;
  - idempotencia del registro de avisos.

## Criterios de aceptación

| Criterio | Estado |
|---|---|
| El orquestador revisa el resultado sin un nuevo mensaje del usuario | Cumplido |
| Funciona para Hermes y para los orquestadores sin sesión | Cumplido (`conversation_id` y bloques) |
| No duplica avisos ni pierde fines de tarea | Cumplido (consumo único + reconciliación) |
| No compite con el usuario ni deja la conversación bloqueada | Cumplido (espera y prioridad del usuario) |
| No puede encadenar turnos automáticos sin fin | Cumplido (`MAX_AUTO_TURNS`) |
| Pruebas automatizadas del comportamiento | Cumplido (8 pruebas nuevas) |
| `./scripts/check.sh` en verde | Cumplido |

## Verificación

```
$ ./scripts/check.sh
EXIT=0
...
test result: ok. 128 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

En el árbol de trabajo había además cambios de otro punto (6), por eso allí el
mismo comando sumaba 134 pruebas del servidor; el commit limpio del punto 7 se
verificó por separado y pasa con 128.

También se verificó el commit aislado en un worktree limpio (sin los cambios de
otros puntos que había en el árbol de trabajo) con el mismo `check.sh`.

## Dudas y límites conocidos

- Hermes depende de que el modelo conserve `?conversation_id=` en la URL del
  `curl` (el prompt se lo indica). Si una sesión antigua no lo incluye, esa
  delegación concreta no genera aviso automático, pero el resto sigue igual.
- El límite de 8 turnos automáticos es deliberadamente bajo y conservador; se
  puede subir en `MAX_AUTO_TURNS` si una cadena legítima lo necesita.
- Las conversaciones siguen viviendo en memoria (pendiente independiente del
  roadmap): al reiniciar el servidor se pierden, y con ellas los avisos en
  curso.
