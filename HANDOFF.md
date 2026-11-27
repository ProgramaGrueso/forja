# Handoff · 2026-11-27 · correcciones del test adversarial y chat

Rama `noche/forja-ui`. Commits locales, **sin push**:

- `c11dec0` correcciones de seguridad + Reintentar.
- `ca74edb` Shift+Enter en el chat, informe y handoff.

Esta actualización de `HANDOFF.md` está **sin commitear**.
Detalle y evidencia: `/home/juang/projects/Forja/docs/adversarial/ENTREGA.md`
(el `ENTREGA.md` de la raíz es de otra tarea).

## Hecho

- 19 de 22 hallazgos corregidos; M1, M2 y M11 parciales.
- Nuevo: reintentar ejecuciones (API y panel).
- Chat del Orquestador: Enter envía, Shift+Enter baja una línea.
- PID 17445 (resto de pruebas) detenido. Ningún servidor de pruebas abierto.
- `./scripts/check.sh` tras los commits: 413 pasan, 0 fallan, 1 ignorado.

## Pendiente (en orden sugerido)

### 1. Ver los cambios en tu instancia

`:7777` corre el binario antiguo (`~/.cargo/bin/forja`). Instalar y reiniciar:

```sh
cd /home/juang/projects/Forja && cargo install --path crates/forja-cli
```

Después, reiniciar la instancia (corta la que está abierta).
Ojo: guardar archivos ahora exige el campo `workspace`; recarga el panel.

### 2. Aplicar la segunda revisión (necesita tu visto bueno: «aplica los pendientes»)

El control de permisos (modo automático, «Undermining Oversight») bloqueó el
comando y no se aplicó nada. Si vuelve a bloquearse, hace falta una regla de
permiso para Bash.

| Cambio | Archivo (`crates/`) | Arreglo |
|---|---|---|
| M11: ``` tras un marcador de lista no abre bloque y la acción siguiente se ejecuta | `forja-server/src/delegation.rs` (`parse_actions`) | Ignorar también lo que `pulldown-cmark` 0.13 (dependencia nueva) marque como bloque de código; mantener las reglas actuales |
| M11: apertura sangrada 2 y «cierre» sangrado 5 cierra el bloque | igual | Lo cubre el mismo cambio |
| Store ilegible se toma como «sin ejecuciones externas» | `forja-sched/src/lib.rs` (`idle_guard`) | `list_runs()` con error = ocupado |
| Línea `  .forja/` se da por excluida | `forja-git/src/lib.rs` (`ensure_excluded`) | Comparar con `trim_end()` |

Cada uno con test; luego `./scripts/check.sh`, `ataques.py`, actualizar
`ENTREGA.md` y revisión por otro modelo.

### 3. Decisiones tuyas

- `opencode` y `hermes` en tareas sensibles: los permites, pero **no** se
  añadieron a los ajustes (la lista sigue solo con `claude`). ¿Los activo?
  Esas tareas saldrían a la nube.
- ¿Aceptas que el token de Hermes vaya dentro del prompt
  (`forja-server/src/orchestrator/chat.rs:27`)?
- ¿Push de la rama?

### 4. Conocido y sin corregir

- M1: sin ejecuciones activas la API sigue abierta.
- M2: tras `SIGKILL` del servidor el agente sigue vivo.
- Chat: si la conversación está ocupada, el mensaje escrito se pierde al enviar.
- El hover de `.btn-primary` casi no se lee (`panel/css/layout.css:500-509`).
- El formulario de Ajustes solo se rellena una vez.
- Sin plazo máximo de apagado; no se limpian worktrees de ejecuciones
  interrumpidas; el prompt es visible en la lista de procesos.
- Otros tests que crean y ejecutan scripts pueden fallar a veces con
  `Text file busy`.
- Sin probar: teclados de Android, Safari anterior a 16, composición IME.

## Comandos

```sh
cd /home/juang/projects/Forja
./scripts/check.sh
cargo build && cd docs/adversarial && python3 ataques.py   # usa el puerto 8081
```
