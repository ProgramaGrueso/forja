# Roadmap de Forja

Pendientes acordados con Juan (26/11/2026). El orden dentro de cada bloque no
implica prioridad: es la lista viva de lo que falta para que Forja sea usable por
alguien que acaba de clonar el repo.

## Experiencia de primer arranque

- **Selector de modelo por agente en el panel.** Hoy los modelos están fijos en
  código (`claude` = sonnet, `agy` = gemini-4-neon-medium, `opencode` =
  opencode-go/deepseek-v4.1-flash, `local` = moe) y solo se puede elegir el
  agente, no su modelo. Cambiar a un campo persistido en Ajustes con override
  por `--model` en el CLI y por tarea.
- **Selector del orquestador en Ajustes.** Hoy el perfil se fija sólo con
  `FORJA_ORCHESTRATOR_PROFILE` al arrancar (lista blanca: default, claudia,
  yuuki) y el modelo es el del perfil de Hermes, fuera de Forja. Debe poder
  elegirse perfil en el panel, con el env var como valor por defecto.
- **Wizard de `forja doctor`** que detecte agentes instalados y proponga una
  configuración mínima que funcione (modelo por agente, perfil del orquestador).

## Conversaciones

- **Varias conversaciones en el orquestador.** Hoy el server guarda una sola
  sesión (`session: Arc<Mutex<Option<String>>>`) y el botón "Nueva" la pisa.
  Falta: mapa `conversación → session_id`, `conversation_id` en
  `/api/orchestrator/chat`, endpoints para listar/crear/borrar y una lista
  lateral con historial por conversación.
- **Comandos de barra ampliados.** Hoy el panel resuelve `/new` y `/help`; falta
  `/model`, `/agent`, `/stop` y que `/new` cree una conversación nueva en vez de
  pisar la única.

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
