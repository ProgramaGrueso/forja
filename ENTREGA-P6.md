# Entrega — punto 6: cierre del ciclo de revisión de worktrees en el panel

## Qué se hizo

- **`forja-git`**: se añadieron las operaciones que faltaban para cerrar el ciclo.
  - `diff_worktree`: diff del worktree desde su `merge-base` con la rama base
    (así no incluye commits que otros worktrees fusionaron después), con `--stat` y parche.
    Incluye archivos sin seguimiento: se marcan temporalmente con `git add -N` y el
    índice se restaura al terminar, así que revisar no deja el worktree modificado.
    El parche se recorta a 512 KiB y el resumen a 64 KiB (cortes seguros en UTF-8).
  - `merge_branch`: exige que el repositorio esté en la rama base y limpio; usa
    `--no-ff`; si hay conflicto ejecuta `merge --abort` y devuelve `Conflict`.
  - `remove_worktree` y `delete_branch`: idempotentes (devuelven `false` si ya no
    existen) y `remove_worktree` limpia registros huérfanos con `worktree prune`.
- **`forja-sched`**: expone `repo_path`, `base_branch` y `worktrees_dir` para que el
  servidor pueda validar y operar sin duplicar configuración.
- **`forja-core`**: nuevo `ForjaError::Conflict`, mapeado a HTTP `409`.
- **Servidor (`review`)**: tres rutas nuevas.
  - `GET /runs/{id}/diff`, `POST /runs/{id}/merge`, `POST /runs/{id}/discard`.
  - Validan: id de ejecución válido, ejecución terminada (no `pending`/`running`),
    rama registrada exactamente `forja/<id>` y distinta de la base, worktree
    exactamente en `<worktrees>/<id>` y dentro de la carpeta configurada (tras
    resolver enlaces, así que un symlink no escapa ni puede apuntar al worktree de
    otra ejecución),
    y servidor con `--repo`.
  - Fusionar exige `done`, commitea antes cualquier cambio pendiente del worktree
    (para que el diff y lo fusionado coincidan), retira el worktree y borra la rama
    solo si queda fusionada. Descartar retira con `--force` y borra la rama con `-D`.
  - Un `review_lock` serializa diff/merge/discard para no operar dos veces el repo.
- **Panel**: el detalle de una ejecución terminada muestra la revisión.
  - Estado, `--stat` y diff con aviso de truncado.
  - «Fusionar en la base» (solo `done`) y «Descartar y limpiar», con confirmación.
  - Errores del servidor visibles y botones que se rehabilitan para reintentar;
    «Reintentar diff» si falla la carga.
  - Si el ciclo ya se cerró (409), lo explica en lugar de mostrar un error opaco.
  - Todo el diff se inserta con `textContent` (sin HTML del contenido revisado).

## Criterios de aceptación

| Criterio | Estado |
|---|---|
| Ver el diff de cada worktree desde el panel | Cumplido (`GET /runs/{id}/diff` + detalle) |
| Fusionar la rama de la tarea desde el panel | Cumplido (solo `done`, base limpia y en su rama) |
| Descartar y limpiar el worktree desde el panel | Cumplido (`done`, `failed` y `cancelled`) |
| Errores visibles y acciones reintentables | Cumplido (errores del servidor + botones) |
| Validaciones de seguridad coherentes con el servidor local | Cumplido (rama `forja/<id>`, worktree confinado, sin `push`) |
| Pruebas automatizadas | Cumplido (backend, integración HTTP y unitarias de validación) |
| `./scripts/check.sh` en verde | Cumplido |

## Verificación

Ejecutado sobre el árbol de trabajo con los cambios del punto 6 y el resto de
puntos ya integrados en `HEAD`:

```
$ CARGO_TARGET_DIR=/tmp/forja-verify-target ./scripts/check.sh
...
test result: ok. 139 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
...
EXIT=0
```

El árbol verificado fue exactamente el commit `104ee5b` (worktree temporal
`/tmp/forja-verify`, sin cambios sin commitear). `node --check` sobre el
JavaScript embebido del panel también pasa.

También se verificó el commit aislado en un worktree temporal
(`/tmp/forja-p6-verify`) con exactamente los archivos del punto 6: fmt, clippy y
todos los tests en verde. `node --check` sobre el JavaScript embebido del panel
también pasa. El código del punto 6 quedó en el commit `104ee5b` («Panel: ciclo de
revisión de worktrees»); esta nota es el único añadido posterior.

Pruebas destacadas añadidas o actualizadas:

- `forja-git`: diff con cambios commiteados, sin commitear y sin seguimiento sin
  alterar el índice; el diff no incluye commits que avanzaron la base después de
  crear el worktree (merge-base); fusión con y sin cambios, rama base incorrecta, árbol sucio
  y conflicto abortado; borrado de worktree/rama idempotente y con `--force`.
- `forja-server` (HTTP): diff→merge cierra el ciclo; descartar borra worktree,
  rama y cambios; ejecución activa/sin repo/inexistente; árbol base sucio;
  `failed` solo se descarta; `cancelled` solo se descarta; cambios pendientes del
  worktree se commitean al fusionar; el diff de un ciclo cerrado da 409.
- `review`: rechazo de ramas que no son `forja/<id>` y confinamiento del worktree
  (incluido un symlink que intenta salir de la carpeta).

## Dudas y límites conocidos

- Fusionar solo se permite en ejecuciones `done`; `failed` y `cancelled` se pueden
  inspeccionar y descartar. Es deliberado: no se fusiona trabajo incompleto sin
  que el usuario lo decida en otro sitio.
- El diff incluye archivos sin seguimiento, pero no muestra el contenido de
  archivos binarios (git imprime «Binary files differ»), igual que `git diff`.
- El panel no ofrece «commitear» por separado: al fusionar se commitea lo
  pendiente con un mensaje fijo. Para un control fino del historial, commitéalo
  en el worktree antes de pulsar «Fusionar».
- Si el repositorio base está en otra rama o sucio, la fusión se rechaza con un
  mensaje claro; no se cambia de rama ni se hace `stash` por el usuario.
- El árbol de trabajo era compartido con otros agentes de la flota; este commit
  incluye solo los archivos del punto 6 (el punto 7 ya estaba integrado en `HEAD`).
