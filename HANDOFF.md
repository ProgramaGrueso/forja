# Handoff · 2026-11-27 · correcciones del test adversarial

Rama `noche/forja-ui`, base `9665946`. Commiteado en local, sin push.
Detalle y evidencia: `/home/juang/projects/Forja/docs/adversarial/ENTREGA.md`
(el `ENTREGA.md` de la raíz es de otra tarea).

## Hecho

- 19 de 22 hallazgos corregidos; M1, M2 y M11 parciales.
- Nuevo: reintentar ejecuciones (API y panel).
- Chat del Orquestador: Enter envía, Shift+Enter baja una línea.
- PID 17445 (resto de pruebas) detenido.
- `./scripts/check.sh`: 413 pasan, 0 fallan, 1 ignorado (3 vueltas).
- `docs/adversarial/ataques.py` sobre el binario corregido: `salida-despues.txt`.

## Pendiente

- Cuatro cambios de la segunda revisión, bloqueados por el control de permisos
  (tabla «Pendiente» de `docs/adversarial/ENTREGA.md`). Requieren tu visto bueno.
- Push (si lo quieres) y reiniciar `:7777` para usar el binario nuevo.
- Decidir: token en el prompt de Hermes. `opencode`/`hermes` permitidos para
  tareas sensibles, pero no añadidos a los ajustes.

## Comandos

```sh
cd /home/juang/projects/Forja
./scripts/check.sh
cargo build && cd docs/adversarial && python3 ataques.py
```
