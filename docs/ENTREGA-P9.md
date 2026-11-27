# Entrega — punto 9: privacidad configurable

## Qué se hizo

- **`Settings.sensitive_agents`** (`crates/forja-server/src/settings.rs`): lista de agentes
  permitidos con privacidad `sensitive`. Por defecto `["claude"]` (estricto). Se valida no
  vacía, solo agentes conocidos y sin duplicados; se normaliza a minúsculas. Un valor
  inválido hace que `PUT /api/settings` responda 400 y no se aplique.
- **Política con lista** (`crates/forja-core/src/policy.rs`): nuevas funciones
  `allowed_agents_with`, `is_agent_allowed_with` y `check_policy_with`; las versiones sin
  lista conservan el comportamiento estricto por defecto (`claude`). `default_sensitive_agents()`
  es el único origen del valor por defecto.
- **Planificador** (`crates/forja-sched`): `SchedulerConfig.sensitive_agents` (default
  `["claude"]`), consultada en `submit` mediante `check_policy_with`, y setter en caliente
  (`set_sensitive_agents`) protegido con `RwLock`. Valida vacío y duplicados.
- **Servidor**: `AppState` aplica la lista al planificador al arrancar y en
  `on_settings_changed`; `GET /api/policy` devuelve la lista efectiva y `PUT /api/settings`
  la persiste.
- **CLI**: `forja run` lee `settings.json` del store y aplica `sensitive_agents` antes de
  encolar; sin archivo usa el default estricto.
- **Panel**: en Ajustes hay un grupo de casillas «Agentes permitidos con privacidad
  sensitive» con los seis agentes, `claude` marcado por defecto y un aviso de que ampliar
  la lista reduce la protección. Se guarda con el resto de ajustes.

## Pruebas

- `forja-core`: default solo `claude`; lista personalizada; vacío, desconocido y duplicados.
- `forja-sched`: el planificador acepta y deniega según la lista y cambia en caliente.
- `forja-server`: `PUT /api/settings` persiste, `GET /api/policy` lo refleja, rechazo 400 y
  validación de la lista; pruebas existentes del panel.
- `forja-cli`: lectura de ajustes y default sin archivo.

Verificación real ejecutada en el worktree:

```
$ ./scripts/check.sh
EXIT=0
forja-agent: 92 passed (1 ignored)
forja-cli: 46 passed
forja-core: 15 passed
forja-sched: 19 passed
forja-server: 151 passed
forja-store: 4 passed
```

## Límites conocidos

- Ampliar `sensitive_agents` es una decisión explícita del usuario: el panel avisa, pero no
  impide seleccionar agentes remotos.
- Los cambios se aplican en caliente en el servidor; la CLI los lee al arrancar cada
  ejecución, así que un cambio durante un `forja run` en curso no afecta a esa ejecución.
- La lista se guarda como parte de `settings.json`; no hay migración porque el campo tiene
  default.
