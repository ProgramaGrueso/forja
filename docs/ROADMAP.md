# Roadmap de Forja

Pendientes acordados con Juan (26/11/2026). El orden dentro de cada bloque no
implica prioridad: es la lista viva de lo que falta para que Forja sea usable por
alguien que acaba de clonar el repo.

## Experiencia de primer arranque

- ~~**Selector de modelo por agente.**~~ **Hecho** (backend + panel). `Settings.agent_models`
  (claude, agy, opencode, local); prioridad override por tarea (`forja run --model`,
  campo `modelo` en `POST /tasks`) > Ajustes > default en código; el modelo efectivo queda
  en `RunRecord.model` y `GET /api/agents` lo devuelve. El panel lo edita en Ajustes
  (tabla agente→modelo con «Restablecer»), tiene el campo opcional «Modelo» en Nueva tarea
  (deshabilitado para mock/hermes) y lo muestra en Agentes y en el detalle de la ejecución.
- ~~**Selector del orquestador en Ajustes.**~~ **Hecho** (backend + panel).
  `Settings.orchestrator_profile` > `FORJA_ORCHESTRATOR_PROFILE` > `default`; se aplica
  en caliente al guardar ajustes (resetea la sesión). Ya no hay lista blanca fija: los
  perfiles de Hermes se descubren (`default` + `<HERMES_HOME o ~/.hermes>/profiles/*`).
- ~~**Ocultos configurables.**~~ **Hecho**: `Settings.hidden_profiles` ∪ `FORJA_HIDDEN_PROFILES`
  (vacío por defecto, sin nombres personales en el código). Un perfil oculto no se lista,
  no se puede elegir y se rechaza en el orquestador, en el agente `hermes` y en `/api/models`.
- ~~**Orquestador independiente de Hermes.**~~ **Hecho**: `Settings.orchestrator_agent`
  (`hermes` | `claude` | `opencode` | `agy` | `local`) > `FORJA_ORCHESTRATOR_AGENT` > primero
  instalado (claude, opencode, agy, local, hermes). Los no-Hermes usan una llamada one-shot
  al CLI con el modelo de `agent_models` y reciben los últimos 20 intercambios en cada
  mensaje. Cambio en caliente; `GET /api/orchestrator/profile` devuelve también `agent`,
  `agents_available` y `profiles`; el panel tiene selector de agente (+ perfil si es hermes)
  y el campo de perfiles ocultos en Ajustes.
- ~~**Wizard de primer arranque.**~~ **Hecho**: `forja init [--yes]` detecta agentes, pregunta
  agente del orquestador, modelo por agente y perfiles de Hermes a ocultar, y fusiona
  `<store_dir>/settings.json` sin pisar campos. `forja doctor` sugiere `forja init` si falta
  el archivo y `forja doctor --fix` lanza el asistente.
- ~~**Orquestador que delega con cualquier agente.**~~ **Hecho**: sin dar permisos de shell,
  los orquestadores no-Hermes terminan su respuesta con bloques `[[forja:delegar {...}]]` /
  `[[forja:estado ID]]` que el servidor valida (mismas reglas que `POST /tasks`, máx. 5
  delegaciones por turno), encola y reinyecta en el siguiente turno («Tareas delegadas»);
  los bloques se ocultan al usuario y solo se leen en la salida del orquestador. Hermes
  conserva el camino `curl`. Límite: el resultado se entrega en el turno siguiente (el
  orquestador no espera a que acaben las tareas).
- ~~**Esfuerzo de razonamiento por familia.**~~ **Hecho**: `Settings.agent_effort`
  (claude, agy, opencode, hermes), `esfuerzo` en `POST /tasks`, `forja run --effort` y
  `forja init`; tarea > ajustes > ninguno (sin configurar no se pasa flag). claude/agy
  `--effort`, hermes `--reasoning` (+ `-m`), opencode `#variante` del modelo; `local` no lo
  soporta. Queda en `RunRecord.effort`; `GET /api/agents` expone `effort`, `effort_levels`
  y `model_editable`.
- ~~**Modelo y esfuerzo por conversación del orquestador.**~~ **Hecho**: desplegables
  `[Modelo] [Esfuerzo]` y micrófono en el composer, `/model` y `/effort`, campos `modelo` y
  `esfuerzo` en la conversación y en `/api/orchestrator/chat`; no reinician la conversación.

## Conversaciones

- ~~**Varias conversaciones en el orquestador.**~~ **Hecho**: mapa
  `conversación → {título, session_id, historial}` (máx. 20 conversaciones y 500
  mensajes c/u, en memoria), `conversation_id` opcional en
  `/api/orchestrator/chat` y `/reset`, y `GET|POST /api/orchestrator/conversations`,
  `GET|DELETE .../conversations/{id}`. El panel tiene lista lateral (por actividad),
  historial por conversación, estado «pensando» independiente por conversación,
  borrar con confirmación, renombrar y selección recordada en `localStorage`.
- ~~**Comandos de barra ampliados.**~~ **Hecho**: `/new`, `/help`, `/rename`, `/agents`,
  `/stop` (el servidor los resuelve); `/model` y `/effort` cambian modelo y esfuerzo de la
  conversación (sin argumento muestran el valor y `reset` vuelve a Ajustes). `/agent` sigue
  reservado y responde «disponible pronto».
  El panel autocompleta al escribir `/`, cambia a la conversación nueva tras `/new` y
  muestra las respuestas de comandos como mensajes del sistema.

## Terminales en paralelo (estilo Orca)

- ~~**Salida en vivo por ejecución.**~~ **Hecho** (backend): `Runner::run_argv_streaming`,
  buffer por run (2000 líneas / 512 KiB) redactado con `forja_core::redact_secrets`,
  `GET /runs/{id}/output?since=SEQ` y SSE `GET /runs/{id}/stream?since=SEQ`.
- ~~**Varios terminales vivos en el panel.**~~ **Hecho**: vista **Terminales** con 1–4 paneles
  (selector de paneles y de columnas), cada uno con la salida en vivo de una ejecución
  (stderr en otro color, autoscroll con pausa al subir, limpiar/cerrar/cancelar, estado),
  selector de ejecución, apertura automática de las tareas nuevas en un panel libre,
  reconexión con `?since=seq` y tope de 5000 líneas en el DOM.

## Pendientes actuales

- **Persistencia de conversaciones.** Las conversaciones del orquestador viven solo en
  memoria (máx. 20 conversaciones y 500 mensajes por conversación) y se pierden al
  reiniciar el servidor. Falta guardarlas en `forja-store` y recuperarlas al arrancar,
  sin romper la selección ni los límites actuales.
- ~~**Cierre del ciclo de revisión en el panel.**~~ **Hecho**: `forja-git` añade
  `diff_worktree` (incluye archivos sin seguimiento sin dejar el índice tocado),
  `merge_branch` (comprueba rama base y árbol limpio, anula la fusión si hay conflicto) y
  `remove_worktree`/`delete_branch` idempotentes. El servidor expone
  `GET /runs/{id}/diff`, `POST /runs/{id}/merge` y `POST /runs/{id}/discard`: validan que
  la ejecución haya terminado, que la rama sea `forja/<id>` y que el worktree cuelgue de la
  carpeta de worktrees; fusionar commitea los cambios pendientes, retira el worktree y
  descartar borra también la rama. El detalle de la ejecución muestra el diff (con
  `--stat`, truncado a 512 KiB) y tiene botones «Fusionar en la base» y «Descartar y
  limpiar» con confirmación, errores visibles y reintento.
- ~~**Reacción del orquestador al terminar las tareas.**~~ **Hecho**: el módulo `reactor`
  escucha el `broadcast` del planificador y, cuando termina una ejecución delegada, da al
  orquestador un turno propio con el resultado resumido (ID, estado, salida y error), sin
  esperar al siguiente mensaje del usuario. Vale para todos los adaptadores: los
  orquestadores sin sesión registran sus bloques `[[forja:delegar]]` y Hermes añade
  `?conversation_id=` a su `curl` de `/tasks`. Se agrupan los fines de tarea de una misma
  conversación, se espera a que el usuario termine si está hablando y una sola reacción
  puede estar en curso por conversación; el registro se consume al primer aviso para que
  el evento SSE y la reconciliación no dupliquen. Tras
  `MAX_AUTO_TURNS` (8) turnos automáticos seguidos, los resultados quedan como «Tareas
  delegadas» para el próximo mensaje del usuario, y este reinicia el contador: las cadenas
  de delegaciones no pueden girar sin fin.
- **División de archivos grandes.** `panel.html` (~3.800 líneas), `orchestrator.rs`
  (~2.900) y `api.rs` (~1.300) concentran demasiado código en un solo archivo. Conviene
  partirlos en módulos (plantilla/JS del panel, comandos y delegación del orquestador,
  rutas por recurso) sin cambiar el comportamiento ni la API.
- **Privacidad configurable.** `Privacy` solo admite `open` y `sensitive` y se elige en
  cada tarea; no hay valor por defecto ni política por agente o repositorio en
  `settings.json` ni en el panel. Falta configurarla y aplicarla de forma consistente en
  `forja run`, `POST /tasks` y las delegaciones del orquestador.
