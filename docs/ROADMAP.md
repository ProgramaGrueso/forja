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

- **Varias conversaciones en el orquestador.** Hoy el server guarda una sola
  sesión (`session: Arc<Mutex<Option<String>>>`) y el botón "Nueva" la pisa.
  Falta: mapa `conversación → session_id`, `conversation_id` en
  `/api/orchestrator/chat`, endpoints para listar/crear/borrar y una lista
  lateral con historial por conversación.
- **Comandos de barra ampliados.** Hoy el panel resuelve `/new` y `/help`; falta
  `/model`, `/agent`, `/stop` y que `/new` cree una conversación nueva en vez de
  pisar la única.

## Terminales en paralelo (estilo Orca)

- **Varios terminales vivos en el panel.** Ver en vivo, y no solo al terminar,
  la salida de cada ejecución; varios a la vez, con selector de foco y scroll
  independiente, como los workspaces de Orca. Hoy la salida se consulta con
  `/runs/ID` y el historial es JSONL append-only.
