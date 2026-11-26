# Forja

Orquestador modular de agentes de código escrito en Rust. Lanza y supervisa tareas sobre worktrees de `git` aislados, aplica políticas de privacidad antes de ejecutar cualquier agente, persiste el historial en formato JSONL append-only y expone una API REST junto a un panel web local autocontenido.

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

Para el detalle de cada crate y sus fronteras, consulta [docs/ARQUITECTURA.md](docs/ARQUITECTURA.md).

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

- `open`: permite `agy`, `mock` y `claude`.
- `sensitive`: solo permite `claude`. Cualquier intento de ejecutar `agy` o `mock` con privacidad `sensitive` se rechaza con `ForjaError::PolicyDenied` antes de crear worktrees o iniciar procesos.
