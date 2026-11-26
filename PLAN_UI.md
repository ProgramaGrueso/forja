# PLAN_UI — Dashboard de escritorio para Forja

Rama: `noche/forja-ui` (desde `noche/forja`). Límite: 3 h desde el inicio registrado en PROGRESS.md (sección "Sesión UI").
Puerta de calidad en cada commit: `./scripts/check.sh`.
Restricciones: sin dependencias Rust nuevas; frontend HTML+CSS+JS vanilla embebido (`include_str!`), sin CDN/npm/build, offline.

## Mínimo obligatorio
- [x] U01 Preparación (15 min): rama, apagar `forja serve` en 7777, PLAN_UI.md, sección en PROGRESS.md, fixture real de `hermes usage`.
      Verificación: `git branch --show-current` = noche/forja-ui y `ps aux | grep "forja.*serve"` vacío.
- [x] U02 Telemetría backend (45 min, subagente): `telemetry.rs` con traits `GpuSource`/`SystemSource`, `nvidia-smi`, `/proc/stat`, `/proc/meminfo`, buffer de 150 muestras (5 min a 2 s).
      Verificación: `cargo test -p forja-server telemetry`.
- [x] U03 Cuotas backend (45 min, subagente): `quota.rs` con trait `UsageCommand` (`hermes usage --provider X --json`), caché 60 s, fallback `~/.config/forja/quota.json`, fixtures.
      Verificación: `cargo test -p forja-server quota`.
- [x] U04 Rutas API (45 min): registro de agentes (proveedor, modelo, disponibilidad), `GET /api/agents`, `GET /api/stats`, `GET /api/policy`, `GET /api/health`, `GET/PUT /api/settings`, `GET /api/telemetry`, `GET/PUT /api/quota`; cableado en `forja serve`.
      Verificación: `cargo test -p forja-server api_`.
- [x] U05 Frontend mínimo (60 min): app a pantalla completa con barra lateral colapsable (logo, pestañas, navegación, agentes, ejecuciones recientes), barra de proveedores fija con popover, mascota SVG reactiva, vistas Telemetría y Ejecuciones en vivo (SSE con reconexión, panel lateral, cancelar).
      Verificación: `cargo test -p forja-server panel`.

## Resto
- [ ] U06 Vistas Resumen, Agentes, Cuotas, Ajustes; modal "Nueva tarea" con política en vivo; filtros; responsive móvil (45 min).
      Verificación: `cargo test -p forja-server panel`.
- [ ] U07 Prueba de humo real en 7788 con curl a todas las rutas + capturas con chromium headless en docs/capturas/ (30 min).
      Verificación: salidas reales pegadas en PROGRESS.md; servidor apagado al final.
- [ ] U08 REPORTE_UI.md (15 min).
      Verificación: `test -s REPORTE_UI.md && ./scripts/check.sh`.
