# Roadmap de Forja

Pendientes acordados con Juan (26/11/2026). El orden dentro de cada bloque no
implica prioridad: es la lista viva de lo que falta para que Forja sea usable por
alguien que acaba de clonar el repo.

## Experiencia de primer arranque

- **[Backend hecho] Selector de modelo por agente.** `Settings.agent_models`
  (claude, agy, opencode, local) persistido; prioridad override por tarea
  (`forja run --model`, campo `modelo` en `POST /tasks`) > Ajustes > default en
  código; el modelo efectivo queda en `RunRecord.model` y `GET /api/agents` lo
  devuelve. Falta pintarlo en `panel.html` (Ajustes y formulario de tarea).
- **[Backend hecho] Selector del orquestador en Ajustes.**
  `Settings.orchestrator_profile` (lista blanca: default, claudia, yuuki)
  > `FORJA_ORCHESTRATOR_PROFILE` > `default`; se aplica en caliente al hacer
  `PUT /api/settings` (resetea la sesión) y `GET /api/orchestrator/profile`
  informa perfil, origen y perfiles elegibles. Falta el selector en el panel.
- **Wizard de `forja doctor`** que detecte agentes instalados y proponga una
  configuración mínima que funcione (modelo por agente, perfil del orquestador).

## Conversaciones

- ~~**Varias conversaciones en el orquestador.**~~ **Hecho en backend**: mapa
  `conversación → {título, session_id, historial}` (máx. 20 conversaciones y 500
  mensajes c/u, en memoria), `conversation_id` opcional en
  `/api/orchestrator/chat` y `/reset` (por defecto `principal`), y
  `GET|POST /api/orchestrator/conversations`, `GET|DELETE .../conversations/{id}`.
  Sin mutex durante la llamada a Hermes. Pendiente: lista lateral en el panel.
- **Comandos de barra ampliados.** **Hecho en servidor**: `/new` (crea
  conversación y devuelve `conversation_id` + `new_conversation`), `/help`,
  `/rename`, `/agents`, `/stop`. `/model` y `/agent` están reservados y responden
  "disponible pronto". Pendiente: usarlos desde el panel.

## Terminales en paralelo (estilo Orca)

- **[Backend hecho] Salida en vivo por ejecución.** `Runner::run_argv_streaming`
  (y `with_output_sink`, que adapta sin cambios a los agentes basados en `Runner`)
  emite líneas stdout/stderr mientras el proceso corre; los agentes sin streaming
  (mock) emiten la salida completa al terminar. El scheduler guarda un buffer por
  run (2000 líneas / 512 KiB), redactado con `forja_core::redact_secrets`, y el
  server expone `GET /runs/{id}/output?since=SEQ` (JSON: `chunks`, `next_seq`,
  `truncated`, `finished`) y SSE `GET /runs/{id}/stream?since=SEQ` (eventos
  `output` y `status` final; si el run terminó reproduce y cierra). Falta la UI:
- **Varios terminales vivos en el panel.** Ver en vivo, y no solo al terminar,
  la salida de cada ejecución; varios a la vez, con selector de foco y scroll
  independiente, como los workspaces de Orca. Hoy la salida se consulta con
  `/runs/ID` y el historial es JSONL append-only.
