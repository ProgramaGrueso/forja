# PLAN — Forja (orquestador de agentes en Rust)

Objetivo: workspace Rust modular que lanza y supervisa agentes de código en
worktrees de git aislados, con política de privacidad y panel web local.
Agente real soportado: `agy` (Gemini). Todo lo demás se prueba con mocks.

Puerta de calidad (se corre al cerrar CADA tarea):
    ./scripts/check.sh
que ejecuta:
    cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
Una tarea solo está "terminada" si check.sh sale con código 0 y su comando propio también.

Dependencias permitidas (solo en Cargo.toml del proyecto): serde, serde_json, thiserror,
tokio, axum, clap, tracing, tracing-subscriber, tempfile (dev), uuid, tower (dev). Otra requiere anotarla en PROGRESS.md.

Estructura final:
    crates/forja-core    tipos, errores, política de privacidad
    crates/forja-git     worktrees vía binario `git`
    crates/forja-agent   trait Agent, MockAgent, AgyAgent, Runner
    crates/forja-store   persistencia de ejecuciones (JSONL)
    crates/forja-sched   planificador con concurrencia y reintentos
    crates/forja-server  API HTTP + panel web embebido
    crates/forja-cli     binario `forja`

## Fase 0 — Cimientos
- [x] T01 Workspace y scripts (20 min)
      Crear workspace con los 7 crates vacíos (lib.rs con un test trivial; cli con main), rust-toolchain
      stable, .gitignore (target/), scripts/check.sh ejecutable.
      Hecho cuando: `./scripts/check.sh` sale 0 y `cargo build --workspace` sale 0.
- [x] T02 forja-core: tipos y errores (30 min)
      Task{id,titulo,prompt,privacidad}, Privacy{Open,Sensitive}, AgentKind{Mock,Agy,Claude},
      RunStatus{Pending,Running,Done,Failed,Cancelled}, ForjaError con thiserror. Serde en todo.
      Hecho cuando: `cargo test -p forja-core` pasa con >=6 tests (ida y vuelta serde, Display de errores).
- [x] T03 forja-core: política de privacidad (30 min)
      fn allowed_agents(Privacy)->Vec<AgentKind>: Sensitive => solo Claude; Open => Agy, Mock, Claude.
      Hecho cuando: `cargo test -p forja-core policy` pasa, con un test que prueba que Sensitive nunca devuelve Agy.

## Fase 1 — Git y agentes
- [x] T04 forja-git: crear worktree (45 min)
      fn create_worktree(repo, base_branch, nueva_rama, destino)->Result<PathBuf>, usando std::process::Command
      con `git`. Tests con tempfile creando un repo real.
      Hecho cuando: `cargo test -p forja-git create` pasa y el test verifica que la rama y la carpeta existen.
- [x] T05 forja-git: estado y commit (30 min)
      fn status_clean, fn commit_all(msg), fn current_branch. Nunca ejecuta push (no debe existir ninguna función con push).
      Hecho cuando: `cargo test -p forja-git` pasa y `grep -rn "push" crates/forja-git/src` no devuelve nada.
- [x] T06 forja-agent: trait Agent y MockAgent (30 min)
      trait Agent { fn kind(&self)->AgentKind; async fn run(&self, ctx:&RunContext)->Result<AgentOutput>; }
      MockAgent configurable: éxito, fallo, tardanza, y escribe un archivo en el worktree.
      Hecho cuando: `cargo test -p forja-agent mock` pasa con casos de éxito, fallo y timeout.
- [x] T07 forja-agent: AgyAgent, construcción del comando (40 min)
      Función pura build_command(opts)->Vec<String> que arma:
      agy -p "<prompt>" --model <modelo> --mode <modo> --output-format json --print-timeout <seg> --add-dir <worktree>
      Sin ejecutar nada. Prompts con comillas y saltos de línea van como argumento, nunca por shell.
      Hecho cuando: `cargo test -p forja-agent agy_args` pasa (>=5 casos, incluido prompt con comillas).
- [x] T08 forja-agent: Runner con agy falso (45 min)
      Runner ejecuta el comando con tokio::process, con timeout y captura de stdout/stderr. Test con
      tests/fake_agy.sh (script que imprime JSON y sale 0/1 según variable de entorno).
      Hecho cuando: `cargo test -p forja-agent runner` pasa; cubre éxito, código distinto de 0 y timeout.
- [x] T09 forja-agent: parseo de salida de agy (30 min)
      Tolerante: si el JSON no parsea, devuelve el texto crudo como AgentOutput y un aviso, sin panic.
      Hecho cuando: `cargo test -p forja-agent parse` pasa con JSON válido, JSON roto y salida vacía.

## Fase 2 — Persistencia y planificador
- [x] T10 forja-store (40 min)
      JSONL append-only en un directorio configurable: append_run, list_runs, get_run. Tolera línea corrupta.
      Hecho cuando: `cargo test -p forja-store` pasa, incluido un test con una línea corrupta en medio.
- [x] T11 forja-sched: cola y concurrencia (50 min)
      Planificador con máximo N ejecuciones simultáneas (por defecto 2), estados persistidos en el store.
      Hecho cuando: `cargo test -p forja-sched concurrency` pasa; un test lanza 6 tareas mock y comprueba
      que nunca hay más de N a la vez.
- [x] T12 forja-sched: reintentos y privacidad (45 min)
      Reintento hasta 2 veces con backoff corto en tests; rechaza antes de ejecutar si la política no permite
      el agente pedido (Sensitive + Agy => error ForjaError::PolicyDenied).
      Hecho cuando: `cargo test -p forja-sched policy retry` pasa; el rechazo ocurre sin crear worktree.

## Fase 3 — Interfaces
- [x] T13 forja-server: API REST (50 min)
      axum: POST /tasks, GET /runs, GET /runs/:id, POST /runs/:id/cancel. Estado en memoria compartido con el scheduler.
      Hecho cuando: `cargo test -p forja-server api` pasa usando tower::ServiceExt (sin abrir puertos).
- [x] T14 forja-server: panel web (40 min)
      Un solo HTML embebido (include_str!), sin CDN ni JS externo; lista de ejecuciones, estado con color, botón cancelar.
      Estética sobria oscura con acentos esmeralda y violeta.
      Hecho cuando: `cargo test -p forja-server panel` verifica que GET / devuelve 200 con content-type text/html
      y que el HTML no contiene "http://" ni "https://" externos.
- [x] T15 forja-cli (40 min)
      clap: `forja run --agent mock|agy --privacy open|sensitive "<prompt>"`, `forja list`, `forja serve --port 7777`.
      Hecho cuando: `cargo test -p forja-cli` pasa y `cargo run -p forja-cli -- list` sale 0 con el store vacío.

## Fase 4 — Cierre
- [x] T16 Prueba de extremo a extremo con mock (45 min)
      Test de integración en crates/forja-cli/tests/e2e.rs: repo temporal, 3 tareas en paralelo con MockAgent,
      cada una en su worktree y rama, commit por tarea, estados correctos en el store.
      Hecho cuando: `cargo test -p forja-cli --test e2e` pasa y verifica que las 3 ramas existen.
- [x] T17 Documentación (30 min)
      README.md (qué es, arquitectura en un diagrama ASCII, cómo correr check.sh, cómo usar agy real),
      docs/ARQUITECTURA.md (un párrafo por crate y sus fronteras).
      Hecho cuando: `test -s README.md && test -s docs/ARQUITECTURA.md` sale 0 y todos los comandos del README
      que sean ejecutables corren sin error.
- [x] T18 Pasada final (30 min)
      `cargo build --workspace --release`, revisión de TODO/unwrap/expect en código no-test (justificar o eliminar),
      REPORTE.md.
      Hecho cuando: `./scripts/check.sh` sale 0 y `grep -rn "todo!\|unimplemented!" crates` no devuelve nada.

## Opcional (solo si sobran tiempo y saldo, en este orden)
- [x] X1 Prueba real con agy, marcada #[ignore]: un prompt trivial en un worktree temporal.
      Hecho cuando: `cargo test -p forja-agent -- --ignored agy_real` pasa (máximo 1 intento).
- [x] X2 Adaptador Claude como stub que devuelve NotConfigured, con test.
- [x] X3 Streaming de estado con Server-Sent Events en /events.
