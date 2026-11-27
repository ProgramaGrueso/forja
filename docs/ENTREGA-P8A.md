# Entrega P8A — Separación modular de `panel.html` manteniendo el embebido

## Resumen

Se refactorizó el archivo monolítico `crates/forja-server/src/panel.html` (4050 líneas al inicio del cambio) separando estructura HTML, hojas de estilo CSS y módulos JS por vista bajo `crates/forja-server/src/panel/`, manteniendo un único HTML embebido en el binario en tiempo de compilación, sin dependencias nuevas, sin peticiones externas y sin archivos sueltos en tiempo de ejecución.

## Estructura resultante

```text
crates/forja-server/
├── build.rs
└── src/
    ├── handlers/
    │   └── panel.rs
    └── panel/
        ├── index.html
        ├── css/
        │   ├── base.css
        │   ├── layout.css
        │   ├── resumen.css
        │   ├── ejecuciones.css
        │   ├── agentes.css
        │   ├── telemetria.css
        │   ├── cuotas.css
        │   ├── ajustes.css
        │   ├── orquestador.css
        │   └── terminales.css
        └── js/
            ├── core.js
            ├── resumen.js
            ├── ejecuciones.js
            ├── agentes.js
            ├── telemetria.js
            ├── cuotas.js
            ├── ajustes.js
            ├── orquestador.js
            └── terminales.js
```

## Ensamblado en compilación

- `crates/forja-server/build.rs` (solo `std`, sin crates adicionales):
  - Declara el orden explícito de `CSS_FILES` y `JS_FILES`.
  - Emite `cargo:rerun-if-changed=...` para `build.rs`, `src/panel/index.html` y cada archivo en `src/panel/css/` y `src/panel/js/`.
  - Lee `src/panel/index.html`, valida que los marcadores `{{CSS}}` y `{{JS}}` aparezcan exactamente una vez, los sustituye por los bloques concatenados en orden y escribe el resultado en `$OUT_DIR/panel.html`.
- `crates/forja-server/src/handlers/panel.rs`:
  - Carga `const PANEL_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/panel.html"));` y sirve `Html(PANEL_HTML)` en `/` sin cambiar la ruta ni el `Content-Type` (`text/html; charset=utf-8`).
- En `src/panel/index.html`, todos los archivos JS se insertan dentro del mismo IIFE `(() => { ... })();` seguido de `bootPanel();`, conservando el mismo ámbito léxico, orden de ejecución e inicialización que el archivo original, sin reescribir lógica ni estilos.

## Líneas por archivo

| Archivo | Líneas | Rol |
|---|---:|---|
| `crates/forja-server/build.rs` | 76 | Ensamblado en compilación hacia `OUT_DIR/panel.html` |
| `crates/forja-server/src/handlers/panel.rs` | 7 | Handler `/` con `include_str!` desde `OUT_DIR` |
| `crates/forja-server/src/panel/index.html` | 572 | Estructura HTML completa + marcadores `{{CSS}}` y `{{JS}}` |
| `crates/forja-server/src/panel/css/base.css` | 49 | Variables `:root`, reset y tipografía base |
| `crates/forja-server/src/panel/css/layout.css` | 597 | Shell, sidebar, navegación, mascota, topbar, botones, modal y responsive |
| `crates/forja-server/src/panel/css/resumen.css` | 48 | Tarjetas KPI y rejillas de Resumen |
| `crates/forja-server/src/panel/css/ejecuciones.css` | 146 | Filtros, tabla de ejecuciones, badges y drawer lateral |
| `crates/forja-server/src/panel/css/agentes.css` | 6 | Rejilla de tarjetas de Agentes (`.grid-cards`) |
| `crates/forja-server/src/panel/css/telemetria.css` | 31 | Barras de progreso (`.meter-*`) y gráficos SVG (`.chart-svg`) |
| `crates/forja-server/src/panel/css/cuotas.css` | 80 | Barra fija de proveedores y popover de cuotas |
| `crates/forja-server/src/panel/css/ajustes.css` | 23 | Campos de formulario (`.form-field`) y avisos (`.toast-banner`) |
| `crates/forja-server/src/panel/css/orquestador.css` | 41 | Layout, lista de conversaciones, chat y voz del Orquestador |
| `crates/forja-server/src/panel/css/terminales.css` | 18 | Rejilla y paneles de Terminales en paralelo |
| `crates/forja-server/src/panel/js/core.js` | 667 | Estado global, utilidades, navegación, sidebar, mascota, modal Nueva tarea, API/SSE y `bootPanel()` |
| `crates/forja-server/src/panel/js/resumen.js` | 83 | Gráfico timeline 24h y vista Resumen |
| `crates/forja-server/src/panel/js/ejecuciones.js` | 267 | Tabla de ejecuciones, drawer de detalle y flujo de revisión/diff |
| `crates/forja-server/src/panel/js/agentes.js` | 65 | Tarjetas de agentes y catálogo de modelos detectados |
| `crates/forja-server/src/panel/js/telemetria.js` | 123 | Gráficos SVG de líneas y renderizado de Telemetría |
| `crates/forja-server/src/panel/js/cuotas.js` | 200 | Barra de proveedores, popover y formulario manual de `quota.json` |
| `crates/forja-server/src/panel/js/ajustes.js` | 242 | Formulario de ajustes, modelos/esfuerzo por agente y perfil del orquestador |
| `crates/forja-server/src/panel/js/orquestador.js` | 520 | Módulo autocontenido del Orquestador (conversaciones, chat, comandos y voz) |
| `crates/forja-server/src/panel/js/terminales.js` | 261 | Módulo autocontenido de Terminales en paralelo (SSE por ejecución) |
| **Total** | **4122** | *(vs. 4050 líneas del `panel.html` monolítico + 7 líneas de `panel.rs`)* |

## Criterios de aceptación y verificación

- [x] `panel.html` monolítico eliminado y reemplazado por `src/panel/` (`index.html`, `css/{base,layout,<vista>}.css`, `js/{core,<vista>}.js`).
- [x] Ensamblado en compilación con `crates/forja-server/build.rs` (solo `std`, sin dependencias nuevas), embebido con `include_str!(concat!(env!("OUT_DIR"), "/panel.html"))`.
- [x] Sin peticiones externas ni archivos sueltos en runtime; ruta `/` y `Content-Type` intactos.
- [x] Prueba existente de IDs (`panel_views_modal_filters_and_settings_present`) mantenida y nueva prueba añadida (`panel_assembled_html_includes_sections_css_and_js_in_order`) verificando secciones HTML, CSS y JS en orden.
- [x] `docs/ROADMAP.md` no modificado; sin `git push`.

### Comandos ejecutados y salida real

1. Validación de sintaxis JS del ensamblado (`node --check`):
   - `JS syntax check (node --check): OK` (exit 0)
2. `./scripts/check.sh` (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`):
   - Exit code: `0`
   - `forja-server`: `152 passed; 0 failed; 0 ignored` (incluyendo `panel_get_root_returns_html_without_external_urls`, `panel_contains_nav_agents_provider_bar_and_mascot_svg`, `panel_views_modal_filters_and_settings_present` y `panel_assembled_html_includes_sections_css_and_js_in_order`).
   - Resto del workspace (`forja-agent`, `forja-cli`, `forja-core`, `forja-git`, `forja-sched`, `forja-store` y doc-tests): todos `ok`, `0 failed`.

## Límites conocidos

- **Ámbito compartido en un único IIFE**: los archivos `panel/js/*.js` no son módulos ES (`import`/`export`) porque el panel se sirve como un único documento HTML embebido sin bundler ni peticiones externas. Siguen compartiendo el closure de `index.html`, por lo que el orden en `JS_FILES` de `build.rs` importa (en particular, `core.js` primero y `ajustes.js` antes de `orquestador.js` por `profileInfo`).
- **Cascada CSS global**: no hay aislamiento automático de selectores por vista; algunos estilos base compartidos (como `.grid-cards` en `agentes.css` o `.form-field`/`.toast-banner` en `ajustes.css`) se reutilizan en otras vistas. El orden de `CSS_FILES` en `build.rs` preserva exactamente la cascada previa.
- **Verificación de DOM en navegador**: las pruebas automatizadas validan el HTML ensamblado, la ausencia de URLs externas, todos los IDs, el orden de secciones/CSS/JS y la sintaxis JS (`node --check`), pero no ejecutan interacción DOM en un navegador headless.
