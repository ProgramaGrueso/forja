---
name: subagent-delegation
description: "Use when splitting work across sub-agents: write self-contained briefs, parallelize safely, verify returns."
version: 1.0.0
license: Same as the Forja repository
---

# Delegar en sub-agentes

Un sub-agente parte de cero: no ve tu conversación, tus archivos abiertos ni tus decisiones previas. La calidad del resultado depende del brief.

## Cuándo delegar
- Trabajo independiente y acotado (investigar, implementar un módulo, revisar un diff, escribir tests).
- Varias líneas paralelas sin ficheros en común.
- Una revisión independiente (contexto limpio) de lo que acabas de producir.

No delegues: decisiones de diseño abiertas, tareas de 2 minutos, ni nada que requiera contexto que no puedes resumir.

## Brief autocontenido
1. **Objetivo** en una frase y el resultado esperado (formato de la respuesta).
2. **Contexto**: ruta exacta del repo/worktree, ficheros relevantes, convenciones, qué ya está hecho.
3. **Restricciones**: qué no tocar, sin dependencias nuevas, sin red, sin `git push`, sin secretos.
4. **Verificación**: comando exacto de tests/lint y qué resultado se espera.
5. **Entrega**: pide resumen corto con ficheros cambiados, salida real de los comandos y dudas. Pide que no invente salidas.

## Paralelismo
- Máx. 3–5 sub-agentes a la vez; cada uno con su rama/worktree si escriben código.
- Particiona por ficheros o módulos, no por «pasos» del mismo fichero.
- Define de antemano los contratos compartidos (nombres, formatos) para que encajen al unir.

## Después de que vuelvan
- Trata el resumen como una afirmación, no como un hecho: verifica con `git diff`, tests y lectura del código (ver `forja-revision`).
- Integra en orden, ejecuta la puerta de calidad completa tras unir.
- Si falla: una corrección acotada con el problema concreto (máx. 2 vueltas), luego escala al usuario.

## En Forja
La delegación se hace con bloques `[[forja:delegar {...}]]` o `POST /tasks`; ver `forja-orquestador` para agentes, privacidad y límites.
