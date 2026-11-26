# ENTREGA — Forja V00 (nuevos tipos de agente)

## Qué se hizo
Tarea **V00** en la rama `noche/forja-V00` (worktree `forja-wt-V00`): nuevos tipos de agente
`Opencode`, `Hermes` y `Local` como base para trabajo en paralelo sin choques.

1. `forja-core/types.rs`: `AgentKind` gana las variantes `Opencode`, `Hermes` y `Local`
   (serde en minúsculas `"opencode"`, `"hermes"`, `"local"`; `Display` y `FromStr` coherentes
   con las existentes, incl. tolerancia a mayúsculas y espacios).
2. `forja-core/policy.rs`: `Open` ahora permite TODOS los agentes
   (`agy, mock, claude, opencode, hermes, local`); `Sensitive` sigue permitiendo SOLO `Claude`.
3. `forja-agent`: nuevos `opencode.rs`, `hermes.rs` y `local.rs` con `OpencodeAgent` (campo
   `model`, por defecto `opencode-go/deepseek-v4.1-flash`), `HermesAgent` (campo `profile`, por
   defecto `yuuki`) y `LocalAgent` (campo `model`, por defecto `moe`); cada uno con `new(...)`,
   `Default`, trait `Agent` y error `NotConfigured` al estilo de `ClaudeAgent`. Exportados con
   `pub mod` / `pub use` en `lib.rs`.
4. `forja-server`: `AppState::new` registra los tres agentes junto a `Mock` y `Agy`
   (`available=true`; `Claude` queda como estaba, sin configurar);
   `AgentMeta::default_for` mapea Opencode → (`OpenCode`, `opencode-go/deepseek-v4.1-flash`),
   Hermes → (`Hermes`, `yuuki`), Local → (`Local`, `moe`), usando las constantes públicas de
   `forja-agent`. `/api/agents` devuelve los 6 y `/api/policy` lista los 6 en `open`.
5. `forja-cli`: `--agent` acepta `opencode|hermes|local` además de `mock|agy`, con ayuda
   actualizada que nombra los 5 agentes.
6. `panel.html` (forja-server): barra lateral estática, filtro `#filter-agent`, select
   `#nt-agent`, banner de política y `state.policy` por defecto incluyen los nuevos nombres;
   el fallback de etiquetas del modal usa un mapa `BASE_AGENT_LABELS` para los 6 agentes.
7. `README.md` y `docs/ARQUITECTURA.md` actualizados donde describían la política y los
   adaptadores (solo lo que quedó desactualizado con el cambio).

## Criterios cumplidos
- [x] `./scripts/check.sh` sale con código **0** (fmt + clippy `-D warnings` + tests).
- [x] `cargo run -q -p forja-cli -- run --help` menciona los 5 agentes
      (`mock, agy, opencode, hermes, local`).
- [x] `Sensitive` solo devuelve `Claude` (test `policy_sensitive_only_returns_claude`).
- [x] `Open` permite todos los agentes (test `policy_open_allows_every_agent` + chequeo de
      `/api/policy`).
- [x] `/api/agents` devuelve los 5 agentes disponibles + `claude` sin configurar
      (test `api_agents_lists_five_available_plus_claude`, 6 entradas).
- [x] Tests nuevos de `FromStr`/`Display`/serde minúsculas para las variantes nuevas.
- [x] Sin dependencias nuevas, sin tocar `Cargo.toml`, sin `push`, sin red en tests, sin
      tocar nada fuera del repo.

## Criterios no cumplidos
- Ninguno.

## Evidencia de verificación (salidas reales)
- `./scripts/check.sh` → exit **0**; `cargo test --workspace`:
  **112 passed; 0 failed; 1 ignored** (`agy_real`, marcado `#[ignore]` como siempre).
  Desglose: forja-agent 26+1 ignored · forja-cli 8 · e2e 1 · forja-core 12 · forja-git 5 ·
  forja-sched 8 · forja-server 48 · forja-store 4.
- `cargo run -q -p forja-cli -- run --help` → exit 0 y muestra:
  `--agent <AGENT>  Agente que ejecuta la tarea: mock, agy, opencode, hermes o local
  [possible values: mock, agy, opencode, hermes, local]`
- Prueba de humo real (puerto 7791, `forja serve`, apagado al terminar):
  - `GET /api/agents` → 200 con `[('claude','Anthropic','sin configurar',False),
    ('agy','Gemini','gemini-4-neon-medium',True), ('hermes','Hermes','yuuki',True),
    ('local','Local','moe',True), ('mock','Local','mock',True),
    ('opencode','OpenCode','opencode-go/deepseek-v4.1-flash',True)]`.
  - `GET /api/policy` → `{"open":["agy","mock","claude","opencode","hermes","local"],
    "sensitive":["claude"]}`.
  - `POST /tasks` con `opencode`+`sensitive` → **403** (política bloquea antes de crear nada).
  - `POST /tasks` con `opencode`+`open` → **201** y la ejecución termina `failed` con
    `no configurado: adaptador opencode aún no configurado` tras 3 intentos (comportamiento
    stub esperado, igual que `ClaudeAgent`).
- `node --check` sobre el JS embebido de `panel.html` → `JS SYNTAX OK`.

## Decisiones tomadas por mi cuenta (para que Juan las revise)
- Orden de `allowed_agents(Open)`: `[agy, mock, claude, opencode, hermes, local]` (los tres
  primeros en el orden previo para minimizar cambios en UI/tests; los nuevos al final).
- Constantes públicas `forja_agent::opencode::DEFAULT_MODEL`,
  `hermes::DEFAULT_PROFILE`, `local::DEFAULT_MODEL` y uso de esas constantes en
  `AgentMeta::default_for`, igual que ya se hacía con `agy::DEFAULT_MODEL`.
- Los tres agentes nuevos se registran como disponibles (`available=true`) aunque su `run`
  devuelva `NotConfigured`: es lo pedido ("junto a Mock y Agy"), así `POST /tasks` los acepta
  y el fallo queda registrado en la ejecución en vez de rechazarse en la puerta HTTP (como sí
  pasa con `claude`, que sigue sin registrarse).
- `ENTREGA.md` se reescribió para esta entrega (el contenido de la entrega anterior queda en
  el historial de git y en `REPORTE.md`/`REPORTE_UI.md`).
- No se añadieron los agentes nuevos al diagrama ASCII del README (por ancho fijo del arte);
  la lista completa vive en la sección de política y en `docs/ARQUITECTURA.md`.

## Dudas abiertas / puntos donde no estoy seguro
- Ninguna bloqueante. Nota: `mock` y `local` comparten proveedor `Local` en `/api/agents`
  (quedan agrupados juntos en la barra lateral del panel); es coherente con los datos pero
  conviene revisarlo visualmente si se prefiere otro etiquetado.
