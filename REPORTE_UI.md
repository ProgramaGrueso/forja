# REPORTE_UI — Dashboard de escritorio para Forja

- **Rama:** `noche/forja-ui` (desde `noche/forja @ 6b488fb`, sin `push`, `main` intacta)
- **Ventana:** `2026-11-26T05:22:31-05:00` → `2026-11-26T06:02:00-05:00` (~40 min de 3 h)
- **Verificación final:** `./scripts/check.sh` -> **exit 0** (`88 passed, 0 failed, 1 ignored`), `cargo build --workspace --release` ok, cero URLs externas (`http://`, `https://`, `<script src`, `<link href`) en `panel.html`.

## Qué se hizo (`U01`–`U08`)
- **Backend modular (sin dependencias nuevas):**
  - `telemetry.rs` (subagente `U02`): lectura real de GPU (`nvidia-smi`), CPU (`/proc/stat`) y RAM (`/proc/meminfo`) detrás de traits `GpuSource` y `SystemSource`, buffer circular de 150 muestras (5 min cada 2 s) y degradación segura a `"GPU no disponible"`.
  - `quota.rs` (subagente `U03`): `UsageCommand` sobre `hermes usage --provider <nombre> --json` (timeout 15 s, caché 60 s, sondeo en paralelo con deduplicación) + fallback manual `~/.config/forja/quota.json` (nunca para Anthropic) y fixtures en `crates/forja-server/tests/fixtures/`.
  - `settings.rs` y `api.rs`: `GET /api/telemetry`, `GET /api/agents`, `GET /api/stats`, `GET /api/policy`, `GET /api/health`, `GET/PUT /api/quota`, `GET/PUT /api/settings`, manteniendo intactos `/tasks`, `/runs`, `/runs/{id}`, `/runs/{id}/cancel` y `/events`.
- **Dashboard offline (`panel.html`, HTML+CSS+JS vanilla embebido):**
  - Tema oscuro casi negro con acentos esmeralda y violeta y estética gótica sobria; responsive móvil (`390x844`).
  - Barra lateral izquierda colapsable (logo, pestañas `EJECUCIONES | AGENTES`, navegación de 6 vistas, agentes agrupados por proveedor con punto de estado vivo y contador de activas, ejecuciones recientes).
  - Barra fija inferior de proveedores estilo Hermes Desktop con anillos SVG de progreso, colores por umbral (`>80 %` ámbar, `>92 %` rojo), `"sin dato"` apagado, popover detallado y estado SSE + versión a la derecha.
  - Mascota chibi pelirroja original en SVG inline reactiva a estados reales (`idle`, `working`, `celebrate`, `worried`), con `prefers-reduced-motion`, globo interactivo al clic y opción de ocultarla en Ajustes.
  - 6 vistas completas (`Resumen`, `Ejecuciones` con SSE y panel lateral deslizable, `Agentes`, `Telemetría` con gráficos SVG de 5 min y guías 80 %/92 %, `Cuotas` con editor de `quota.json`, `Ajustes`) y modal `Nueva tarea` con política `Sensitive` en vivo.
- **Auditoría adversaria aplicada:** bloqueo de `..`, `\0` y rutas protegidas (`.hermes`, `.ssh`, `.gnupg`, `.env`) en `PUT /api/settings`; rechazo de tareas con título/prompt vacíos (`400`); sondeo paralelo de cuotas (`1.40 s` en frío vs ~5.5 s); corrección del clic de la mascota, del refresco DOM cada 1 s y de la cabecera móvil.

## Qué quedó fuera / observaciones
- Nada del plan quedó pendiente.
- **Mejoras recomendadas a futuro:** (1) autenticar el dashboard si se expone fuera de `127.0.0.1` (hoy escucha solo en loopback); (2) limitar tamaño de cuerpo HTTP (`DefaultBodyLimit`) en `POST /tasks`; (3) reconectar cambios de `max_concurrency` en caliente sin reiniciar `forja serve`; (4) conectar `ClaudeAgent` a un CLI/API real cuando Juan decida el protocolo.

## Decisiones y dependencias
- **Dependencias nuevas:** ninguna (`0` añadidas en Cargo ni en frontend).
- **Subagentes lanzados:** 2 (`U02` y `U03`, ambos aceptados con `check.sh` en verde).
- **Capturas verificadas (`docs/capturas/`):** `01-resumen.png`, `02-ejecuciones.png`, `03-telemetria.png`, `04-cuotas.png`, `05-agentes.png`, `06-ajustes.png`, `07-movil.png` (todas con datos reales de RTX 4070 Ti SUPER y `hermes usage` Anthropic; servidor en `7788` apagado tras las pruebas).
