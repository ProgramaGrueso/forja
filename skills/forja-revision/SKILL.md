---
name: forja-revision
description: "Use when reviewing an agent's result (diff, output, tests) before declaring a task done."
version: 1.0.0
license: Same as the Forja repository
---

# Forja: checklist para revisar el resultado de un agente

Revisa con evidencia propia, no con el resumen del agente. Un resumen describe lo que el agente *cree* que hizo.

## 1. ¿Hizo lo pedido?
- [ ] Relee la tarea original y compara punto por punto con el diff (`git diff <base>...<rama>`, `git diff --stat`).
- [ ] No falta nada pedido y no hay cambios fuera de alcance (ficheros ajenos, refactors no pedidos, formato masivo).
- [ ] Si la tarea era ambigua, ¿qué suposición tomó el agente? ¿Es razonable?

## 2. ¿Funciona de verdad?
- [ ] Ejecuta tú la puerta de calidad del proyecto (tests, lint, formato, build) y lee la salida real y el código de salida. No te fíes de «los tests pasan».
- [ ] Los tests nuevos fallan sin el cambio y pasan con él; cubren el caso de error, no solo el feliz.
- [ ] Sin tests deshabilitados, `skip`, `#[ignore]`, aserciones debilitadas o resultados inventados.
- [ ] Sin dependencias nuevas no pedidas.

## 3. Seguridad y privacidad
- [ ] Sin secretos, tokens, claves, `.env`, rutas personales, nombres de personas, hosts o IDs en código, tests, docs o mensajes de commit.
- [ ] Sin llamadas de red o agentes reales en tests; datos de prueba falsos.
- [ ] Entradas externas validadas (rutas, comandos, JSON); sin inyección de shell/SQL, sin `path traversal`.
- [ ] No borra ni sobrescribe datos existentes sin que se pidiera; nada escribe fuera del área de trabajo.

## 4. Calidad
- [ ] Sigue las convenciones del repo (estilo, nombres, estructura de módulos, idioma de mensajes).
- [ ] Manejo de errores explícito; sin `unwrap`/`panic` en caminos de usuario; sin código muerto ni comentarios obsoletos.
- [ ] Cambio mínimo y legible; documentación y README actualizados si cambia el comportamiento.

## 5. Git
- [ ] Commits en la rama de la tarea, mensaje claro, sin ficheros generados ni artefactos.
- [ ] Nunca `git push` ni cambios en la rama principal sin permiso del usuario.

## 6. Veredicto
Responde con una de tres: **APROBADO** (todo verificado), **CAMBIOS** (lista concreta y acotada de lo que falta, para una corrección), o **RECHAZADO** (enfoque erróneo: replantear). Incluye siempre: comandos ejecutados y su resultado real, hallazgos con fichero:línea, y lo que no pudiste verificar.
