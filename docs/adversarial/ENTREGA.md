# Test adversarial de Forja y correcciones

Rama `noche/forja-ui`, base `9665946`. Commiteado en local, **sin push**. Última actualización: 2026-11-27.

- [Correcciones](#correcciones) · estado actual, evidencia y lo que queda
- [Informe adversarial original](#informe-adversarial-original) · cómo estaba antes

## Correcciones

### Resultado

De 22 hallazgos: **19 corregidos y 3 parciales (M1, M2, M11)**. Función nueva: **Reintentar** una ejecución (`POST /runs/{id}/retry` y botón en el panel).

Quién produjo el código: un agente Opus (núcleo), un agente Sonnet (web y panel) y la sesión principal (ajustes tras las revisiones).

| ID | Estado | Qué hace ahora | Límite |
|---|---|---|---|
| A1 | Corregido | No escribe en `.git`, `.forja`, store ni `settings.json` (comprueba la ruta canónica) | |
| A2 | Corregido | Rechaza `/`, `$HOME` y sus ancestros, carpetas ocultas de `$HOME` y de sistema; `/run/media` permitido | `/usr`, `/var` y otras no están en la lista. El repo de arranque está exento |
| A3 | Corregido | 409 con HEAD suelto u otra rama; no borra nada | |
| A4 | Corregido | `local` solo acepta modelos `llm-local/...` en tareas sensibles | |
| A5 | Corregido | El orquestador no permitido no recibe título, salida ni error | |
| **M1** | **Parcial** | 409 al cambiar `sensitive_agents`, `quota_path` u `orchestrator_agent` con ejecuciones activas; el token no se hereda | Sin ejecuciones activas la API sigue abierta; el token va en el prompt de Hermes (`orchestrator/chat.rs:27`); un agente de tu usuario puede editar archivos |
| **M2** | **Parcial** | SIGTERM/SIGINT/SIGHUP cierran los agentes; al arrancar, las ejecuciones huérfanas pasan a `failed` | Tras `SIGKILL` del servidor el agente sigue vivo (`t16b`) |
| M3 | Corregido | Grupo de procesos: cancelar y timeout matan a los nietos | Un nieto que haga `setsid` escapa |
| M4 | Corregido | `.forja/` va a `.git/info/exclude` | |
| M5 | Corregido | Rutas de datos absolutas | |
| M6 | Corregido | No sobrescribe un archivo que no sea de cuotas | `PUT /api/settings` aún acepta esa ruta; lo que se rechaza es la escritura |
| M7 | Corregido | `PUT /api/files/write` **exige `workspace`** (409 si cambió); el panel confirma | Cambio de API: clientes antiguos reciben 400 |
| M8 | Corregido | Conserva permisos | |
| M9 | Corregido | Bloqueo de archivo y salto de líneas ilegibles | |
| M10 | Corregido | Redacta antes de guardar; el agente recibe el original | Un token suelto con forma de UUID no se redacta |
| **M11** | **Parcial** | Ignora acciones en bloques ``` / ~~~ y con sangría | **Quedan 2 evasiones** (ver «Pendiente») |
| B1 | Corregido | 409 claro si la ejecución es de otro repo; un repo renombrado se recupera con `git worktree repair` | |
| B2 | Corregido | Aviso, copia del archivo corrupto y `PUT` parcial conserva claves | Mezcla solo el primer nivel; el aviso dura hasta reiniciar |
| B3 | Corregido | Un prompt que empieza por `-` ya no es una opción | |
| B4 | Corregido | Prompt ≤100 KiB, salida ≤256 KiB, error ≤64 KiB | |
| B5 | Corregido | Archivo 0600 en carpeta 0700 propia del usuario | |
| B6 | Corregido | Búsqueda con límites y `truncated` | |

La sospecha original «la fusión commitea antes de comprobar la base» también quedó corregida.

### Criterios de aceptación

| # | Criterio | Estado |
|---|---|---|
| 1 | Cada hallazgo bloqueado al repetir el ataque, o parcial con motivo | **Cumplido con parciales:** M1, M2, M11. Las 2 evasiones de M11 las vi leyendo el código; `t08` no las cubre |
| 2 | Altos y medios con test de regresión en Rust | Cumplido (para M11, solo de los casos corregidos) |
| 3 | `scripts/check.sh` pasa | Cumplido |
| 4 | Reintentar funciona por API y panel, verificado en navegador | Cumplido (la prueba de navegador es anterior a los ajustes tras revisión, que no tocan el panel) |
| 5 | Los `runs.jsonl` y `settings.json` antiguos cargan; tu instancia intacta | Cumplido por tests y `serde(default)`; no probé con tus archivos reales. `:7777` no se tocó |
| 6 | Revisión por otro modelo y evidencia aquí | Cumplido, pero **sus últimos hallazgos no están aplicados** |

### Evidencia

`./scripts/check.sh` (fmt, clippy `-D warnings`, `cargo test --workspace`), tres veces seguidas sobre el código actual:

```
vuelta 1: check.sh exit 0
413 pasan, 0 fallan, 1 ignorados
vuelta 2: check.sh exit 0
413 pasan, 0 fallan, 1 ignorados
vuelta 3: check.sh exit 0
413 pasan, 0 fallan, 1 ignorados
```

Ignorado: `agy_real_trivial_prompt_in_temp_worktree` (requiere `agy` real y cuota).

Antes de eso hubo un fallo intermitente (`Text file busy`) en el test de procesos nietos; ahora ejecuta el script vía `/bin/sh`. Otros tests que crean y ejecutan scripts podrían sufrir la misma carrera.

`python3 ataques.py` sobre el binario corregido: código 0, salida completa en [`salida-despues.txt`](salida-despues.txt) (antes: [`salida.txt`](salida.txt)). Actualicé el script para que envíe `workspace` al guardar, de modo que los ataques se rechacen por la protección y no por falta del campo. Cómo leer lo que puede parecer un fallo:

| Prueba | Línea | Significado |
|---|---|---|
| t02 | `escribir .bashrc: 200` | La carpeta no cambió (`$HOME` dio 400): escribió en el repo. El `.bashrc` real sigue `# original` |
| t03 | `escribir 'config' ...: 200` | Igual: archivo en la raíz del repo; `.git/config` intacto |
| t06 | tarea `failed` | Su llamada a la API fue rechazada; la política no cambió |
| t16b | `proceso original sigue vivo: true` | Límite conocido de M2 |

Comprobaciones aparte, hechas antes de los ajustes tras revisión (sus scripts eran temporales y no se conservan):

- Protecciones con la API nueva: escritura en `.git/config`, symlinks, `.git` de un worktree y `.forja` → 400; `README.md` → 200.
- Panel en Chromium sin ventana: 25 comprobaciones, 0 fallos (reintentar con un solo POST tras triple clic, formulario precargado si el prompt fue redactado, avisos, 409, cambios sin guardar, sin errores de JS).
- B3 con binario real: **abrió una sesión de `opencode` en un modelo gratuito** (`big-pickle`) en una carpeta temporal. No envió datos privados.

### Revisión cruzada (`opencode-go/glm-5.3`; recibió el diff y el código)

Un primer intento con `deepseek-v4-pro` sobre el diff completo agotó el tiempo sin responder.

**Ronda 1**, corregido con test: cierre de bloque con sangría (M11), escritura no atómica de `info/exclude`, zombi contado como vivo, cambio de carpeta que no veía `forja run` externos, repo movido bloqueado. Falsos positivos comprobados en el código: redacción de UUID tras `Bearer`, `hide_secret` sin usar, falta de `serde(default)`, prompt alterado, aviso sin conectar, filtro de pestañas, contención de rutas, interbloqueo.

**Ronda 2** (sobre esas correcciones): ver «Pendiente».

### Pendiente

Intenté aplicar estos cambios y **el control de permisos de Claude Code (modo automático, motivo «Undermining Oversight») bloqueó el comando**. No lo reintenté por otra vía. Comprobé que no se aplicó nada.

| Qué | Dónde | Arreglo propuesto |
|---|---|---|
| M11: ``` ``` ``` tras un marcador de lista (`- ```` ```) no abre bloque, y la acción de la línea siguiente se ejecuta | `forja-server/src/delegation.rs`, `parse_actions` | Ignorar además toda línea que un analizador CommonMark (`pulldown-cmark`, dependencia nueva) marque como bloque de código |
| M11: con apertura sangrada 2, un «cierre» sangrado 5 cierra el bloque | igual | Lo cubre el mismo cambio |
| Si el store no se puede leer, se asume que no hay ejecuciones externas | `forja-sched/src/lib.rs`, `idle_guard` | Tratar el error como «ocupado» |
| Una línea `  .forja/` (con espacios delante) se da por excluida | `forja-git/src/lib.rs`, `ensure_excluded` | Comparar con `trim_end()` |

Descartados de la ronda 2:

- «Ramas con nombre repetido entre repos»: los IDs son UUID v4 (`forja-core/src/types.rs:114`). Lo mismo para registros antiguos sin repo ni worktree.
- Zombis y PID reciclado sin `/proc`: solo fuera de Linux.
- Apertura de bloque con sangría ≥4 que ignora acciones posteriores: falla hacia no ejecutar.
- Carrera entre procesos al cambiar de carpeta y `hidepid`: límites que acepto y declaro.

### Caja del chat: Shift+Enter (pedido posterior)

La caja del Orquestador pasó de `input` a `textarea`: **Enter envía, Shift+Enter baja una línea**, crece hasta 160 px y a ancho de móvil ocupa su propia fila.

| Criterio | Estado |
|---|---|
| Shift+Enter baja de línea sin enviar | Cumplido |
| Enter envía una vez y conserva los saltos; vacío no envía | Cumplido |
| Autocompletar, botón Enviar y «Citar en Orquestador» siguen funcionando | Cumplido |
| Usable a 390 px | Cumplido (la primera versión dejaba la caja en 26 px; corregido) |

Chromium sin ventana contra una instancia aislada con agente falso: 16 comprobaciones, `fallos: 0`. `./scripts/check.sh`: 413 pasan, 0 fallan, 1 ignorado.

Revisión de GLM 5.3: un fallo real (`line-height` anulado por `font`), corregido y vuelto a probar. Descartados por la prueba: historial sin saltos de línea y caja que desborda en escritorio. Sin probar: teclados de Android (ahí queda el botón Enviar), Safari anterior a 16 y composición IME. Defecto anterior que sigue: si la conversación está ocupada, el mensaje escrito se pierde al enviarlo.

### Dudas y decisiones tuyas

- Dijiste que `opencode` y `hermes` pueden recibir tareas sensibles (envían datos a la nube). El código ya permite añadirlos a `sensitive_agents`; **no cambié tus ajustes**, que siguen solo con `claude`.
- ¿Aceptas el token de Hermes dentro del prompt?
- Tu instancia en `:7777` ejecuta el binario antiguo hasta que la reinicies.
- El PID 17445 (`forja serve` sobre un repo temporal) se detuvo con tu permiso.
- Defectos de interfaz vistos y no corregidos: el hover de `.btn-primary` casi no se lee (`panel/css/layout.css:500-509`) y el formulario de Ajustes solo se rellena una vez.
- Sin tratar: no hay plazo máximo de apagado; los worktrees de ejecuciones interrumpidas no se limpian; el prompt es visible en la lista de procesos.
- Los diffs y fragmentos de código se enviaron a modelos de `opencode-go` (DeepSeek, GLM) para revisarlos.

### Cómo comprobarlo

```sh
cd /home/juang/projects/Forja
./scripts/check.sh
cargo build
cd docs/adversarial && python3 ataques.py    # usa el puerto 8081 en t05
```

`ss -ltnp` tras las pruebas: de Forja solo escucha `:7777` (tu instancia, PID 369293).

---

## Informe adversarial original

Fecha: 2026-11-26. Commit `9665946`. Binario: `target/debug/forja`. Describe el estado **antes** de las correcciones.

### Qué se hizo

- Lectura del código de los crates y 26 pruebas dinámicas (`ataques.py`) contra instancias aisladas: repo, `HOME`, XDG y `TMPDIR` temporales, agentes falsos en el `PATH`, sin gastar créditos. En `t09` se ejecutaron además `opencode` y `claude` reales con `--version`.
- Prueba del panel en navegador con datos hostiles (`panel.py`).
- En esa fase no se corrigió nada.

5 hallazgos altos, 11 medios y 6 bajos, todos reproducidos. La severidad es juicio mío.

**Alcance de los ataques por API:** una web externa quedó bloqueada (Host/Origin/Sec-Fetch-Site). Los hallazgos que usan la API requieren un proceso local que llegue al puerto (incluido un agente lanzado por Forja) o quien use el panel. El token de autenticación está apagado por defecto y `forja app` no lo activa.

Rutas relativas a `crates/`.

### Altos

| ID | Hallazgo y evidencia | Prueba | Dónde |
|---|---|---|---|
| A1 | Se salta la protección de `.git`: con un symlink `g -> .git`, `PUT /api/files/write g/config` añade `core.fsmonitor`, y **la propia fusión de Forja ejecuta el comando**. También sirve seleccionar `<repo>/.git` y escribir `config` (200). | t03, t03b | `forja-server/src/files.rs:175` |
| A2 | `POST /api/workspaces/select` acepta cualquier carpeta (`$HOME`, `/`): se leyó una `.ssh/id_rsa` falsa y se sobrescribió `.bashrc`. | t02 | `forja-server/src/workspaces.rs:191` |
| A3 | Pérdida silenciosa de trabajo: si el agente deja HEAD suelto en el worktree, el diff muestra el archivo, la fusión responde `merged:true, already_up_to_date:true`, borra worktree y rama, y el archivo no está en `main`. | t12 | `forja-server/src/review.rs:201` |
| A4 | Con `local` en `sensitive_agents`, `modelo: "openrouter/vendor/modelo-nube"` manda el prompt `sensitive` a ese proveedor. No es la configuración por defecto. | t05 | `forja-agent/src/local.rs:39` |
| A5 | La salida de una tarea `sensitive` se envía al orquestador aunque su agente no esté permitido. | t07 | `forja-server/src/delegation.rs:213` |

### Medios

| ID | Hallazgo y evidencia | Prueba |
|---|---|---|
| M1 | Una tarea `open` llama a la API y añade `opencode` a `sensitive_agents` (antes 403, después 201). | t06 |
| M2 | SIGTERM deja el agente vivo; al reiniciar la ejecución sigue `running` y bloquea cambio de carpeta, fusión y descarte. | t16, t16b |
| M3 | Cancelar no mata a los nietos: uno escribió en el worktree después. | t15 |
| M4 | En un repo sin `.forja/` en `.gitignore`, toda fusión falla. | t11 |
| M5 | `serve --repo X` desde otra carpeta: la tarea falla y deja worktree y rama. | t10 |
| M6 | `quota_path` puede apuntar a `~/.claude/settings.json`, y `PUT /api/quota` lo reemplaza. | t19 |
| M7 | El editor puede escribir en otro proyecto; el panel descarta cambios sin preguntar. | t14, navegador |
| M8 | Guardar desde el editor cambia permisos a 0644. | t04 |
| M9 | Un byte no UTF-8 en `runs.jsonl` impide arrancar; escrituras simultáneas corrompen líneas. | t17, t18 |
| M10 | `/runs/{id}.output` y `runs.jsonl` conservan secretos en claro. | t20 |
| M11 | Una orden `[[forja:delegar ...]]` dentro de un bloque de código citado se ejecuta. | t08 |

### Bajos

| ID | Hallazgo | Prueba |
|---|---|---|
| B1 | Tras cambiar de carpeta, fusionar da 409 confuso y descartar responde 200 sin borrar nada. | t13 |
| B2 | `settings.json` corrupto: arranca con valores por defecto sin aviso. Un `PUT` parcial vacía `hidden_profiles`. | t19 |
| B3 | Un prompt que empieza por `-` se toma como opción. | t09 |
| B4 | Prompt de 200 KiB falla con `Argument list too long`; 20 MB de salida dan un `GET /runs` de 20 MB. | t01, t20 |
| B5 | El archivo de consulta de Hermes se crea 0644 en `TMPDIR`. | t21 |
| B6 | Con carpeta `/`, una búsqueda sin resultados tarda unos 6,5 s. | t24 |

### Defensas que aguantaron

- Host, Origin, `Origin: null` y cross-site ajenos: 403. `text/plain`: 415.
- `..`, `%2e%2e`, ruta absoluta y symlink a `/etc`: 400. `run_id` con traversal: 404. Cuerpo de 300 KiB: 413.
- Con `--auth-token`, las rutas protegidas probadas dan 401.
- Fuzz de 1500 peticiones: 0 panics.
- `skills install --force` no toca symlinks.
- XSS: ningún payload se ejecutó en las 9 vistas, el detalle de ejecución, un archivo hostil ni el modal de carpetas (`salida-navegador.json`). El navegador no confirma que cada payload llegara a pintarse en cada vista.

### Corrección hecha durante las pruebas

Afirmé que un proceso mío llevaba 9 minutos al 96 % de CPU por una búsqueda abandonada. No estaba verificado y `t24` lo desmintió. Era la fase de fuzz con muchas búsquedas seguidas.
