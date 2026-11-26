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
  `Settings.orchestrator_profile` (lista blanca: default, claudia, yuuki) >
  `FORJA_ORCHESTRATOR_PROFILE` > `default`; se aplica en caliente al guardar ajustes
  (resetea la sesión). El panel ofrece el selector en Ajustes y en la cabecera de
  Orquestador, con el origen del valor y el aviso de reinicio de sesiones.
- **Wizard de `forja doctor`** que detecte agentes instalados y proponga una
  configuración mínima que funcione (modelo por agente, perfil del orquestador).

## Conversaciones

- ~~**Varias conversaciones en el orquestador.**~~ **Hecho**: mapa
  `conversación → {título, session_id, historial}` (máx. 20 conversaciones y 500
  mensajes c/u, en memoria), `conversation_id` opcional en
  `/api/orchestrator/chat` y `/reset`, y `GET|POST /api/orchestrator/conversations`,
  `GET|DELETE .../conversations/{id}`. El panel tiene lista lateral (por actividad),
  historial por conversación, estado «pensando» independiente por conversación,
  borrar con confirmación, renombrar y selección recordada en `localStorage`.
- ~~**Comandos de barra ampliados.**~~ **Hecho**: `/new`, `/help`, `/rename`, `/agents`,
  `/stop` (el servidor los resuelve; `/model` y `/agent` responden «disponible pronto»).
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
