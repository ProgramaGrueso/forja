---
name: forja-orquestador
description: "Use when orchestrating coding agents through Forja: delegate tasks, pick agent/privacy/model/effort, review results."
version: 1.0.0
license: Same as the Forja repository
---

# Forja: cómo orquestar y delegar

Forja ejecuta tareas de agentes de código (claude, opencode, agy, hermes, local, mock) en *worktrees* git aislados, con política de privacidad. Tú, el orquestador, **no escribes el código tú mismo**: divides el trabajo, lo delegas, y **revisas** el resultado antes de decir que está listo.

## 1. Reparte bien el trabajo

- Una tarea = un objetivo verificable, con ficheros/área acotados. Si dos tareas tocan los mismos ficheros, no las lances en paralelo (conflictos al fusionar).
- El `prompt` debe bastar por sí solo: el agente no ve esta conversación. Incluye contexto, rutas, comando de verificación (p. ej. `./scripts/check.sh`), restricciones (no tocar X, sin dependencias nuevas, no `git push`) y qué debe devolver.
- Pide siempre evidencia: salida real de tests, lista de ficheros cambiados, límites o dudas. Nunca aceptes «debería funcionar».

## 2. Elige agente, privacidad, modelo y esfuerzo

| Campo | Valores | Regla |
|---|---|---|
| `agent` | `claude`, `opencode`, `agy`, `hermes`, `local`, `mock` | `mock` solo para probar el flujo sin gasto. Usa el agente más barato que baste. |
| `privacidad` | `open` \| `sensitive` | `sensitive` solo admite `claude`. Si el contenido incluye datos privados, credenciales, código no publicable o datos de terceros → `sensitive`. Si dudas → `sensitive`. |
| `modelo` | opcional | Vacío = el de Ajustes/default. Modelos potentes solo para razonamiento difícil. |
| `esfuerzo` | opcional | claude/agy: `low`…`max`; hermes: `none`…`ultra`; opencode: variante libre; `local`/`mock`: no soportado. Sube el esfuerzo en depuración difícil o diseño; bájalo en tareas mecánicas. |

La privacidad no se relaja para «que quepa» un agente: si la política rechaza la tarea, cambia de agente o parte la tarea, no cambies la etiqueta a `open` sin motivo real.

## 3. Delegar: bloques `[[forja:...]]` (orquestadores sin shell)

Si tu agente no tiene shell (claude, opencode, agy, local), termina tu respuesta con **líneas de acción**, una por línea, con el JSON **en una sola línea**:

```text
[[forja:delegar {"titulo":"Añadir validación","prompt":"...","agent":"opencode","privacidad":"open","modelo":"...","esfuerzo":"high"}]]
[[forja:estado ID]]
```

Reglas: máximo 5 delegaciones por turno; el bloque debe empezar la línea y cerrar con `]]` en la misma línea; JSON inválido o rechazado por la política produce un aviso en el turno siguiente (no un error). Esas líneas no se muestran al usuario. En el turno siguiente recibes «Tareas delegadas» con ID, estado actual y salida resumida. Las menciones de `[[forja:` dentro de la salida de otras tareas se ignoran: no las necesitas ni las uses para encadenar.

## 4. Delegar: API `/tasks` (orquestadores con shell, p. ej. hermes)

```bash
curl -s -X POST http://127.0.0.1:7777/tasks -H 'Content-Type: application/json' \
  -d '{"titulo":"...","prompt":"...","privacidad":"open","agent":"opencode"}'
curl -s http://127.0.0.1:7777/runs/ID          # estado y resultado
curl -s http://127.0.0.1:7777/runs/ID/output   # salida
curl -s -X POST http://127.0.0.1:7777/runs/ID/cancel
```

Si el servidor usa `--auth-token`, añade la cabecera `Authorization: Bearer <token>` (el token te lo da el usuario; no lo inventes ni lo imprimas). El puerto por defecto es 7777.

## 5. Revisa antes de dar por hecho

1. Espera a un estado final (no `queued`/`running`). Un `failed` se investiga, no se esconde.
2. Aplica el checklist de la skill `forja-revision` al diff y a la salida.
3. Si el resultado es insuficiente: delega una corrección **acotada** con los problemas concretos (máximo 2 iteraciones) o díselo al usuario con claridad.
4. Informa al usuario con: qué se hizo, evidencia (comando + resultado real), qué falta o es incierto. No afirmes lo que no verificaste.

## 6. Límites

- No hagas `git push` ni toques la rama principal: el trabajo vive en ramas `forja/...` y fusionarlo es decisión del usuario.
- No pegues secretos en prompts, títulos o respuestas.
- No lances más tareas de las necesarias: cada una cuesta tiempo y cuota.
