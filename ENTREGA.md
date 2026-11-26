# ENTREGA — Forja V03 (adaptador real de Hermes)

## Qué se hizo
Tarea **V03** en la rama `noche/forja-V03` (worktree `forja-wt-V03`): reemplazo del stub
`NotConfigured` de `crates/forja-agent/src/hermes.rs` por el adaptador real del CLI `hermes`,
siguiendo `agy.rs` como modelo (función pura `build_command`, `Runner` con timeout y captura,
parseo tolerante, errores `ForjaError` en español).

1. `hermes.rs` (reescrito):
   - `build_command(&HermesCommandOptions) -> Vec<String>` pura que arma
     `hermes -p <perfil> chat --query-file <archivo> -Q --oneshot` sin ejecutar nada. Flags
     confirmadas con la ayuda real: `-p <perfil>` está en el epílogo de `hermes --help`
     ("Run any command against a named profile's home (also --profile)"; se consume antes de
     argparse, por eso no sale en la línea de uso) y `--query-file PATH`, `-Q/--quiet` y
     `--oneshot` están en `hermes chat --help`. Ninguna flag inventada.
   - `validate_profile(&str)`: normaliza (trim + minúsculas, igual que hace el propio hermes
     con `-p`), valida el nombre (solo letras, números, guion y guion bajo, sin empezar por
     guion/guion bajo, máximo 64 caracteres — espeja la regex interna de hermes
     `^[a-z0-9][a-z0-9_-]{0,63}$`, verificada en su fuente) y aplica la lista blanca
     `ALLOWED_PROFILES = ["default", "claudia", "yuuki"]`. "intimo" y "mama" (y cualquier
     otro, incl. "INTIMO"/"MaMa" en mayúsculas) se rechazan con `ForjaError::InvalidInput`
     ANTES de crear el archivo temporal o el proceso; el mensaje nombra el perfil rechazado
     y lista los permitidos.
   - El prompt viaja en `forja-hermes-prompt-<uuid v4>.txt` creado en
     `std::env::temp_dir()` y se borra SIEMPRE: paso explícito al final de `run` (con aviso
     en `warnings` si el borrado falla), limpieza best-effort si falla la escritura del
     archivo, y `PromptFileGuard` (Drop) que cubre la cancelación a mitad de ejecución
     (`Scheduler::cancel_run`). Sin shell: argv directo por `Runner::run_argv`, cwd del
     proceso = `ctx.worktree`.
   - `parse_hermes_output`: tolerante y sin panic. Contrato verificado en la fuente de
     hermes (`cli_single_query.py`, modo quieto): la respuesta final va a stdout y
     `session_id:` a stderr, así que stdout ya es la respuesta; si está vacía → texto vacío
     + aviso "salida vacía de hermes (revisa raw_stderr)" en `warnings`.
   - Errores claros en español: programa ausente (`ErrorKind::NotFound` → `ForjaError::Agent`
     "no se encontró el programa ... (¿está instalado y en el PATH?)"), código ≠ 0 y timeout
     vía `Runner` (mensaje existente en español).
   - `HermesAgent` mantiene la API previa (`Default`, `new(profile)`, campo `profile`,
     `DEFAULT_PROFILE`) y añade campo `program` (defecto "hermes") + `with_program()` para
     que los tests apunten al script falso. No hizo falta tocar `lib.rs`.
2. `tests/fake_hermes.sh` (nuevo, ejecutable): falso `hermes` sin red ni LLM; verifica la
   forma del argv, informa `query_file:`/`cwd:` en stderr (para que los tests comprueben
   cwd y borrado del temp) y se dirige por el contenido del prompt: `FAKE:fail` (stderr +
   código 1), `FAKE:timeout` (duerme 30 s), `FAKE:empty` (stdout vacío, código 0), o eco
   literal del archivo (prueba el prompt verbatim).
3. `crates/forja-agent/Cargo.toml` y `Cargo.lock`: UNA línea `uuid.workspace = true`
   (ver "Decisiones").
4. `PROGRESS.md`: entrada de la sesión V03 (memoria del proyecto).

## Criterios cumplidos
- [x] `./scripts/check.sh` sale con código **0** (fmt + clippy `-D warnings` +
      `cargo test --workspace`).
- [x] ≥6 tests propios (11 en total, todos en verde): forma exacta de `build_command`
      (ruta con espacios y comillas), argv[0] reemplazable, acepta la lista blanca y
      normaliza, rechaza desconocidos/privados, rechaza nombres inválidos, parseo
      texto/vacío, éxito con script falso, código ≠ 0, timeout, salida vacía, programa
      inexistente, perfiles privados rechazados antes de ejecutar nada.
- [x] El prompt con comillas, saltos de línea y metacaracteres (`$(whoami)`, backticks)
      llega verbatim por el archivo temporal — nunca por shell — y el test lo afirma con
      igualdad exacta.
- [x] El archivo temporal se borra en éxito, en código ≠ 0 y en salida vacía (los tests
      lo verifican con la ruta que reporta el script falso); además guard por Drop para
      cancelación y limpieza si falla la escritura. Verificado también fuera de los
      tests: `ls /tmp/forja-hermes-prompt-*` no devuelve nada tras la suite.
- [x] "intimo" y "mama" se rechazan ANTES de ejecutar nada (test con el script falso
      apuntado: el error es `InvalidInput` del whitelist, sin rastro de proceso).
- [x] Ningún test llama a la red ni ejecuta `hermes`/`agy`/`claude`/`opencode` reales; solo
      `tests/fake_hermes.sh` (bash local). El único test que toca un CLI real sigue siendo
      `agy_real`, marcado `#[ignore]` como siempre.
- [x] Sin `git push`, sin tocar `main`, sin tocar nada fuera del repo, sin borrar archivos
      ajenos.

## Criterios no cumplidos
- Ninguno.

## Evidencia de verificación (salidas reales)
- `cargo test -p forja-agent` → `test result: ok. 37 passed; 0 failed; 1 ignored`
  (antes de V03 eran 26 + 1 ignored; +11 tests de hermes, 0 fallidos).
- `./scripts/check.sh` → **exit 0**. Desglose real de `cargo test --workspace`:
  **123 passed; 0 failed; 1 ignored** (`agy_real`, `#[ignore]` como siempre) —
  forja-agent 37 (+1 ignored) · forja-cli 8 · e2e 1 · forja-core 12 · forja-git 5 ·
  forja-sched 8 · forja-server 48 · forja-store 4.
- Flags confirmadas con la ayuda real (única ejecución permitida del CLI, sin gasto):
  `hermes chat --help` muestra `--query-file PATH` ("Read the single query from a file
  instead of the command line... Safe for arbitrary text"), `--oneshot` ("With
  -q/--query-file: answer the query and exit") y `-Q, --quiet` ("Quiet mode for
  programmatic use... Only output the final response and session info"). El epílogo de
  `hermes --help` documenta `hermes -p <profile> <cmd>` ("Run any command against a named
  profile's home (also --profile)").
- `hermes profile list` (solo lectura) confirma los perfiles reales: default, claudia,
  intimo, mama, yuuki → la lista blanca pedida excluye exactamente los privados, y
  coincide con `HIDDEN_HERMES_PROFILES` que el panel ya oculta.
- `ls /tmp/forja-hermes-prompt-*` tras la suite → "No such file or directory" (0 archivos
  temporales rezagados).

## Decisiones tomadas por mi cuenta (para que Juan las revise)
1. **`uuid.workspace = true` en `crates/forja-agent/Cargo.toml`** (única desviación de "no
   toques otro archivo"): la tarea pedía expresamente "nombre único con uuid" para el
   archivo temporal; `uuid` ya figura en `[workspace.dependencies]` del proyecto, está en
   la lista de dependencias permitidas de PLAN.md y ya se compila en el grafo vía
   forja-core y forja-sched (el lock no añade ningún crate nuevo: uuid 1.29.3 ya estaba).
   Si prefieres no tocar el Cargo.toml, el cambio a pid+nanos es trivial y te lo aplico.
2. **"build_command con prompt con comillas y saltos de línea"** se interpretó así: en
   hermes el prompt NO viaja en el argv (va por `--query-file`, que es más seguro); por
   eso la prueba de comillas/saltos/metacaracteres es de extremo a extremo (éxito con
   script falso + igualdad exacta con el prompt), y además hay test de `build_command`
   con una ruta de archivo con espacios y comillas.
3. **Normalización del perfil** (trim + minúsculas): espeja el comportamiento real de
   hermes (`scan_profile_flag` hace strip+casefold), así "Yuuki" funciona y "INTIMO"
   sigue bloqueándose. El charset exige además que el nombre no empiece por `-`/`_` y
   mida ≤ 64, copiado de la regex propia de hermes.
4. **Salida vacía → `Ok` con aviso en `warnings`**, no error: el proceso terminó bien
   (código 0) y agy.rs trata igual el caso; el error de fondo, si lo hay, queda en
   `raw_stderr`.
5. **`parse_hermes_output` no intenta parsear JSON**: el formato texto de
   `-Q --oneshot` ya entrega la respuesta cruda en stdout; meter extracción JSON
   podría confundir una respuesta JSON legítima del modelo con un sobre del CLI.
6. **Guard de borrado (`PromptFileGuard`)**: extra a lo pedido, cubre la cancelación a
   mitad de ejecución (el scheduler tiene `cancel_run`); es 10 líneas y no cambia el
   flujo normal.

## Quién revisó (regla de Juan: nunca revisa quien produce)
- Revisión automática: `./scripts/check.sh` (fmt, clippy `-D warnings`, 123 tests) en
  verde, repetida tras cada cambio y tras aplicar las correcciones del revisor.
- Revisión independiente: subagente adversarial con OTRO modelo. Se intentó primero con
  `opencode/claude-fable-5-2` (falló por saldo insuficiente del proveedor opencode) y se
  lanzó con `opencode-go/kimi-k2.7-code`. **VEREDICTO: ACEPTA** ("la implementación es
  segura, bien probada y cumple la especificación"), re-ejecutando por su cuenta
  `hermes --help`, `hermes chat --help`, `cargo test -p forja-agent`,
  `cargo clippy -p forja-agent --all-targets -- -D warnings` y `./scripts/check.sh`,
  todo en verde. Hallazgos y disposición:
  - (menor) Ventana de cancelación durante la escritura del temp: **corregido** — el
    `PromptFileGuard` ahora se crea ANTES de escribir el archivo, no después.
  - (menor) Longitud del perfil contada en bytes: **corregido** — ahora cuenta caracteres
    (`chars().count() <= 64`), idéntico a la regex de hermes. (En la práctica era
    inofensivo: todo nombre que pasa el charset es ASCII puro, bytes == caracteres.)
  - (menor) Doble fallo (comando Err + borrado Err) no deja aviso: **aceptado sin cambio**
    — no hay `AgentOutput` al que adjuntarlo y el guard reintenta el borrado; complicar el
    error para un caso de doble fallo improbable no mejora la entrega.
  - (cosmético) `Drop` con E/S síncrona: **aceptado sin cambio** — un `unlink` en /tmp
    tarda microsegundos; una limpieza async coordinada añadiría complejidad real.
  - (cosmético) El mensaje de nombre inválido no lista los permitidos: **aceptado sin
    cambio** — mezclaría dos causas distintas (formato vs whitelist); el mensaje ya dice
    exactamente qué está mal ("solo letras, números, guion y guion bajo").
  - Falsos positivos que el revisor descartó por su cuenta: redundancia guard/remove,
    supuesta falta de test del prompt verbatim, posible flakiness por paralelismo, uso de
    `-p` y posible inyección por shell.

## Dudas abiertas / puntos donde no estoy seguro
- Ninguna bloqueante. Dos observaciones honestas:
  1. No ejecuté `hermes chat` real con un prompt (prohibido gastar saldo): la forma del
     argv está validada contra `--help` + la fuente del CLI, y el comportamiento contra el
     script falso. La primera ejecución real de Juan (`forja run --agent hermes ...`) es la
     prueba de fuego definitiva.
  2. Con `-Q` real, hermes imprime en stderr también el `session_id:`; el parseo lo ignora
     (solo usa stdout) y queda preservado en `raw_stderr` por si se necesita retomar la
     sesión. Si algún día aparece un caso en stdout, el texto crudo lo preserva igual.
- Pendiente de integración (fuera del alcance permitido de V03): `README.md` y
  `docs/ARQUITECTURA.md` aún describen hermes como stub `NotConfigured` (la tarea prohibía
  tocar otros archivos; V01/V02 harán lo mismo con opencode/local y conviene actualizar la
  documentación una sola vez al integrar las tres ramas).
