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

- **Varios terminales vivos en el panel.** Ver en vivo, y no solo al terminar,
  la salida de cada ejecución; varios a la vez, con selector de foco y scroll
  independiente, como los workspaces de Orca. Hoy la salida se consulta con
  `/runs/ID` y el historial es JSONL append-only.
