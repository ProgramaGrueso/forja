# Forja

![Panel de Forja](docs/capturas/01-resumen.png)

Orquestador modular de agentes de código escrito en Rust. Lanza y supervisa tareas sobre worktrees de `git` aislados, aplica políticas de privacidad antes de ejecutar cualquier agente, persiste el historial en formato JSONL append-only y expone una API REST junto a un panel web local autocontenido.

## Inicio rápido

Requisitos: [Rust](https://rustup.rs) y `git`. Los agentes (`claude`, `opencode`, `agy`, `hermes`, `llm`) son opcionales; con `mock` puedes probar sin instalar nada.

```bash
git clone <url-del-repo> forja && cd forja
./install.sh            # compila e instala el binario `forja`
forja doctor            # revisa git, agentes instalados, repo y puerto

cd /ruta/a/tu-repo      # Forja detecta el repo y su rama actual solo
forja serve --open      # panel en http://127.0.0.1:7777
forja run --agent mock --privacy open "Crear archivo de prueba"
```

Sin instalar, desde el repo de Forja: `cargo run -p forja-cli -- doctor`.

### Qué hace cada comando

- `forja doctor`: diagnostica tu entorno y te dice qué falta.
- `forja serve --open`: levanta el panel web local (solo accesible desde tu máquina).
- `forja run --agent AGENTE --privacy open|sensitive "tu tarea"`: ejecuta una tarea y muestra el resultado.
- `forja list`: historial de ejecuciones.

Cada tarea trabaja en un *worktree* (copia aislada) del repo en su propia rama `forja/...`; tu rama de trabajo no se toca hasta que tú decidas fusionar.

### Si algo falla

- `cargo: command not found`: instala Rust desde https://rustup.rs.
- `forja: command not found` tras instalar: añade `~/.cargo/bin` a tu `PATH`.
- Puerto ocupado: `forja serve --port 7788`.
- Un agente aparece como no instalado: instálalo o usa `--agent mock`.

Los datos (historial y worktrees) se guardan en `.forja/` dentro de donde ejecutes el comando; está en `.gitignore`.

## Arquitectura

```text
                       +-------------------+
                       |     forja-cli     |  (binario `forja`: run | list | serve)
                       +----+---------+----+
                            |         |
                            v         v
                  +---------------+  +-------------------+
                  | forja-server  |  |    forja-sched    |  (cola, semáforo N, reintentos, política)
                  | API REST + UI |->+---+---+---+---+---+
                  +---------------+      |   |   |   |
                                         |   |   |   +------------------+
                         +---------------+   |   +----------+           |
                         v                   v              v           v
                 +--------------+    +-------------+  +-----------+  +------------+
                 |  forja-git   |    | forja-agent |  |forja-store|  | forja-core |
                 | (worktrees)  |    | Mock / Agy  |  |  (JSONL)  |  |tipos/policy|
                 +------+-------+    +------+------+  +-----+-----+  +------------+
                        |                   |               |              ^
                        +-------------------+---------------+--------------+
```

Para el detalle de cada crate y sus fronteras, consulta [docs/ARQUITECTURA.md](docs/ARQUITECTURA.md); el historial de desarrollo está en `docs/historial/`.

## Puerta de calidad

Ejecuta formato, `clippy` estricto (`-D warnings`) y toda la suite de pruebas (sin red ni gasto de saldo):

```bash
./scripts/check.sh
```

Compilación completa del workspace y prueba de extremo a extremo con `MockAgent`:

```bash
cargo build --workspace
cargo test -p forja-cli --test e2e
```

## Uso básico

Listar ejecuciones registradas (sale con código `0` cuando el store está vacío):

```bash
cargo run -p forja-cli -- list
```

Ejecutar una tarea con `MockAgent`:

```bash
cargo run -p forja-cli -- run --agent mock --privacy open --title "Demo" "Crear archivo de prueba"
```

Levantar la API REST y el panel web local (por defecto en `http://127.0.0.1:7777`):

```bash
cargo run -p forja-cli -- serve --port 7777
```

## Uso con `agy` real

Forja invoca el binario `agy` directamente mediante `tokio::process::Command` (nunca a través de una shell) pasando el prompt como argumento íntegro:

```bash
cargo run -p forja-cli -- --repo /ruta/al/repo --base-branch main \
  run --agent agy --privacy open --title "Refactor" "Explica el módulo principal"
```

### Política de privacidad

- `open`: permite todos los agentes: `agy`, `mock`, `claude`, `opencode`, `hermes` y `local` (cada uno requiere su CLI instalado; `forja doctor` te dice cuáles tienes).
- `sensitive`: solo permite `claude`. Cualquier intento de ejecutar otro agente con privacidad `sensitive` se rechaza con `ForjaError::PolicyDenied` antes de crear worktrees o iniciar procesos.
