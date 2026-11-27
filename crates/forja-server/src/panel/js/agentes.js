      /* Vista Agentes */
      function renderAgentsView() {
        $("agents-cards-grid").innerHTML = state.agents.map((ag) => {
          const rate = ag.success_rate != null ? `${(ag.success_rate * 100).toFixed(0)} %` : "sin dato";
          const avg = ag.avg_duration_ms != null ? fmtDuration(ag.avg_duration_ms) : "sin dato";
          return `
            <div class="card card-gothic">
              <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;">
                <div style="display:flex;align-items:center;gap:8px;">
                  <span class="dot ${esc(ag.state)}"></span>
                  <strong style="font-size:15px;">${esc(ag.name)}</strong>
                </div>
                <span class="mono muted" style="font-size:11.5px;">${esc(ag.provider)}</span>
              </div>
              <div class="kv-grid" style="margin-top:8px;">
                <span class="muted">Modelo:</span><span class="mono">${esc(ag.model)}${state.settings && state.settings.agent_models && state.settings.agent_models[ag.name] ? " (de Ajustes)" : ""}</span>
                <span class="muted">Esfuerzo:</span><span class="mono">${esc(ag.effort || ((ag.effort_levels || []).length ? "sin configurar" : "sin soporte"))}${ag.effort ? " (de Ajustes)" : ""}</span>
                <span class="muted">Estado:</span><span class="mono">${esc(ag.state)}</span>
                <span class="muted">Ejecuciones:</span><span class="mono">${ag.total} (activas: ${ag.active})</span>
                <span class="muted">Tasa de éxito:</span><span class="mono">${esc(rate)}</span>
                <span class="muted">Duración media:</span><span class="mono">${esc(avg)}</span>
              </div>
              ${ag.last_error ? `<div class="card-sub" style="color:#fca5a5;margin-top:8px;">Último fallo: ${esc(ag.last_error)}</div>` : ""}
            </div>`;
        }).join("");
      }

      function renderModels() {
        const grid = $("models-grid");
        if (!state.models) { grid.innerHTML = '<div class="card card-gothic muted">Detectando modelos…</div>'; return; }
        grid.innerHTML = state.models.providers.map((p) => {
          const ok = p.status === "ok";
          const items = ok
            ? p.models.map((m) => `<li><span class="mono">${esc(m.id)}</span>${m.label && m.label !== m.id ? ` <span class="muted" style="font-size:11.5px;">${esc(m.label)}</span>` : ""}</li>`).join("")
            : "";
          return `
            <div class="card card-gothic">
              <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;">
                <div style="display:flex;align-items:center;gap:8px;">
                  <span class="dot ${ok ? "idle" : "error"}"></span>
                  <strong style="font-size:14px;">${esc(p.display_name)}</strong>
                </div>
                <span class="mono muted" style="font-size:11.5px;">${ok ? p.models.length + " modelos" : "sin datos"}</span>
              </div>
              ${ok
                ? `<ul style="margin:0;padding-left:16px;max-height:190px;overflow:auto;font-size:12.5px;line-height:1.6;">${items}</ul>`
                : `<div class="card-sub" style="color:#fca5a5;">${esc(p.error || "no disponible")}</div>`}
              <div class="card-sub mono muted" style="margin-top:8px;font-size:11px;">fuente: ${esc(p.source)}</div>
            </div>`;
        }).join("");
      }

      function loadModels(refresh) {
        return apiFetch(refresh ? "/api/models?refresh=true" : "/api/models")
          .then((r) => r.ok ? r.json() : null)
          .then((m) => { if (m) { state.models = m; renderModels(); orqHooks.refresh(); } })
          .catch(() => {});
      }

      $("models-refresh").addEventListener("click", () => {
        $("models-refresh").textContent = "Detectando…";
        loadModels(true).then(() => { $("models-refresh").textContent = "Redetectar"; });
      });
      renderModels();
      loadModels(false);
