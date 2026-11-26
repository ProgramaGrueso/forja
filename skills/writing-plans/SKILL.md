---
name: writing-plans
description: "Use when a task needs a plan first: break work into small verifiable steps before touching code."
version: 1.0.0
license: Same as the Forja repository
---

# Escribir planes

Planifica antes de implementar cuando el trabajo toca varios ficheros, tiene decisiones de diseño o se va a repartir entre agentes. Si cabe en un cambio de 10 líneas, no hace falta plan.

## Proceso
1. **Entiende el objetivo**: una frase de qué resultado observable se espera y cómo se comprobará. Si falta información decisiva, pregunta antes de planear.
2. **Explora lo mínimo**: lee el código y los docs afectados; anota rutas y funciones reales (no inventes nombres).
3. **Lista restricciones**: compatibilidad, no tocar X, sin dependencias nuevas, rendimiento, privacidad.
4. **Divide en pasos pequeños** (idealmente 2–10 min cada uno), cada uno con: fichero(s), cambio concreto, y **cómo verificarlo** (comando + resultado esperado).
5. **Ordena por dependencias** y marca qué pasos son independientes (candidatos a paralelizar/delegar). Pasos que tocan los mismos ficheros no se paralelizan.
6. **Riesgos y preguntas abiertas**: qué puede salir mal, qué se decide después, qué se deja fuera de alcance.
7. **Criterio de «hecho»**: puerta de calidad completa en verde + los comportamientos observables del punto 1.

## Formato

```markdown
# Plan: <título>
Objetivo: ...
Fuera de alcance: ...
## Pasos
1. [ ] <cambio> — ficheros: ... — verificar: `<comando>` → <resultado>
2. [ ] ...
## Riesgos / preguntas
- ...
## Hecho cuando
- ...
```

## Reglas
- Tests primero cuando aplique (ver `test-driven-development`).
- Un paso, una verificación. Sin pasos tipo «implementar todo».
- Actualiza el plan si la realidad lo contradice; no lo sigas a ciegas.
- Guarda el plan donde el usuario lo pida (p. ej. `docs/` o el PR), nunca con datos personales.
