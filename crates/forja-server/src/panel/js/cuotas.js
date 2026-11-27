      /* Barra de proveedores y popover */
      function formatProviderChipText(p) {
        if (p.status !== "ok") return `${p.display_name} sin dato`;
        if (p.balance != null) {
          const sym = p.currency === "USD" ? "$" : "";
          const cur = p.currency ? ` ${p.currency}` : "";
          return `${p.display_name} ${sym}${Number(p.balance).toFixed(2)}${cur}`;
        }
        if (p.used_percent != null) {
          const rem = Math.max(0, Math.min(100, Math.round(100 - p.used_percent)));
          return `${p.display_name} ${rem}%`;
        }
        return `${p.display_name} sin dato`;
      }

      function ringSvg(usedPct, status) {
        if (status !== "ok") {
          return `<svg class="ring-svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="5" fill="none" stroke="#4b5563" stroke-width="2"/></svg>`;
        }
        if (usedPct == null) {
          return `<svg class="ring-svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="4.5" fill="#10b981"/></svg>`;
        }
        const pct = Math.max(0, Math.min(100, usedPct));
        const color = pct > 92 ? "#ef4444" : pct > 80 ? "#f59e0b" : "#10b981";
        const circ = 2 * Math.PI * 5.5;
        const dash = ((100 - pct) / 100) * circ;
        return `<svg class="ring-svg" viewBox="0 0 16 16">
          <circle cx="8" cy="8" r="5.5" fill="none" stroke="#1f2937" stroke-width="2.2"/>
          <circle cx="8" cy="8" r="5.5" fill="none" stroke="${color}" stroke-width="2.2"
            stroke-dasharray="${dash.toFixed(1)} ${circ.toFixed(1)}" transform="rotate(-90 8 8)" stroke-linecap="round"/>
        </svg>`;
      }

      function renderProviderBar() {
        const providers = (state.quota && state.quota.providers) || [];
        if (providers.length === 0) return;
        $("provider-chips").innerHTML = providers.map((p) => {
          const lvl = p.status === "ok" ? (levelClass(p.used_percent) || "ok") : "";
          const cls = p.status === "ok" ? `provider-chip level-${lvl}` : "provider-chip dimmed";
          return `<button class="${cls}" data-provider="${esc(p.provider)}" type="button">
            ${ringSvg(p.used_percent, p.status)}
            <span>${esc(formatProviderChipText(p))}</span>
          </button>`;
        }).join("");

        if (state.openPopoverProvider) {
          renderProviderPopover(state.openPopoverProvider);
        }
      }

      function renderProviderPopover(providerId) {
        const pop = $("provider-popover");
        const providers = (state.quota && state.quota.providers) || [];
        const p = providers.find((x) => x.provider === providerId);
        if (!p) {
          pop.classList.remove("open");
          return;
        }
        const srcLabel = p.source === "hermes" ? "hermes usage --json" : p.source === "manual" ? "quota.json manual" : "sin fuente disponible";
        const winHtml = (p.windows && p.windows.length > 0)
          ? p.windows.map((w) => {
              const used = w.used_percent != null ? `${w.used_percent.toFixed(1)}% usado (${Math.max(0, 100 - w.used_percent).toFixed(0)}% libre)` : "sin dato";
              return `<div style="margin-top:6px;padding-top:6px;border-top:1px solid var(--border-dim);font-size:12px;">
                <div style="display:flex;justify-content:space-between;"><strong>${esc(w.label)}</strong><span class="mono">${esc(used)}</span></div>
                <div class="muted" style="font-size:11px;">Reinicio: ${esc(w.resets_at || "sin dato")}</div>
              </div>`;
            }).join("")
          : `<div class="muted" style="font-size:12px;margin-top:6px;">${
              p.status === "ok" && p.balance != null
                ? `Saldo registrado: ${esc(p.balance)} ${esc(p.currency || "")}`
                : p.status === "ok" && p.used_percent != null
                  ? `Restante: ${(100 - p.used_percent).toFixed(1)}% · Usado: ${p.used_percent.toFixed(1)}%`
                  : esc(p.error || "Sin datos disponibles")
            }</div>`;

        pop.innerHTML = `
          <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:6px;">
            <strong style="font-size:13.5px;">${esc(p.display_name)}</strong>
            <button id="close-popover-btn" class="icon-btn" style="width:22px;height:22px;" type="button">✕</button>
          </div>
          <div class="kv-grid" style="grid-template-columns:105px 1fr;">
            <span class="muted">Fuente:</span><span class="mono">${esc(srcLabel)}</span>
            <span class="muted">Reinicio:</span><span class="mono">${esc(p.resets_at || "sin dato")}</span>
            <span class="muted">Actualizado:</span><span>${esc(p.updated_at ? relAgeFromIso(p.updated_at) : "sin dato")}</span>
            <span class="muted">Última lectura:</span><span class="mono">${esc(relAgeFromMs(p.fetched_at_ms))}</span>
          </div>
          ${winHtml}
        `;
        pop.classList.add("open");
        const closeBtn = $("close-popover-btn");
        if (closeBtn) {
          closeBtn.onclick = () => {
            state.openPopoverProvider = null;
            pop.classList.remove("open");
          };
        }
      }

      /* Vista Cuotas */
      let manualQuotaRenderedOnce = false;
      function renderQuotasView(forceForm) {
        const providers = (state.quota && state.quota.providers) || [];
        $("quota-cards-grid").innerHTML = providers.map((p) => {
          const used = p.used_percent;
          const lvl = levelClass(used);
          const headline = p.status !== "ok"
            ? "sin dato"
            : p.balance != null
              ? `${p.currency === "USD" ? "$" : ""}${Number(p.balance).toFixed(2)} ${p.currency || ""}`
              : used != null
                ? `${Math.max(0, 100 - used).toFixed(0)} % libre (${used.toFixed(1)} % usado)`
                : "sin dato";
          const wins = (p.windows || []).map((w) => {
            const wPct = w.used_percent || 0;
            return `<div style="margin-top:8px;">
              <div style="display:flex;justify-content:space-between;font-size:11.5px;">
                <span>${esc(w.label)}</span>
                <span class="mono">${w.used_percent != null ? `${w.used_percent.toFixed(1)}% usado` : "sin dato"}</span>
              </div>
              <div class="meter-track"><div class="meter-fill ${levelClass(wPct)}" style="width:${Math.min(100, wPct)}%"></div></div>
              <div class="muted" style="font-size:10.5px;">Reinicio: ${esc(w.resets_at || "sin dato")}</div>
            </div>`;
          }).join("");

          return `
            <div class="card card-gothic">
              <div style="display:flex;justify-content:space-between;align-items:center;">
                <strong style="font-size:14.5px;">${esc(p.display_name)}</strong>
                <span class="mono muted" style="font-size:11px;">origen: ${esc(p.source)}</span>
              </div>
              <div class="card-value" style="font-size:20px;margin:8px 0 4px;">${esc(headline)}</div>
              ${used != null && p.windows.length === 0 ? `<div class="meter-track"><div class="meter-fill ${lvl}" style="width:${Math.min(100, used)}%"></div></div>` : ""}
              ${wins}
              <div class="card-sub" style="margin-top:8px;">
                ${p.updated_at ? `Actualizado ${esc(relAgeFromIso(p.updated_at))}` : (p.error ? esc(p.error) : "Sin dato")}
              </div>
            </div>`;
        }).join("");

        if (!manualQuotaRenderedOnce || forceForm) {
          const manual = (state.quota && state.quota.manual && state.quota.manual.providers) || {};
          const editable = ["copilot", "deepseek", "nous", "gemini"];
          $("manual-quota-form-rows").innerHTML = editable.map((id) => {
            const e = manual[id] || {};
            return `
              <div class="card" style="padding:10px 12px;">
                <div style="font-weight:700;margin-bottom:6px;text-transform:capitalize;">${id}</div>
                <div class="form-field" style="margin-bottom:6px;">
                  <label>% restante (0–100, opcional)</label>
                  <input class="input mono mq-rem" data-prov="${id}" type="number" step="0.1" min="0" max="100" value="${e.percent_remaining ?? ""}">
                </div>
                <div class="form-field" style="margin-bottom:6px;">
                  <label>Saldo (opcional, ej. 78.24)</label>
                  <input class="input mono mq-bal" data-prov="${id}" type="number" step="0.01" min="0" value="${e.balance ?? ""}">
                </div>
                <div class="form-field" style="margin-bottom:0;">
                  <label>Moneda (ej. USD)</label>
                  <input class="input mono mq-cur" data-prov="${id}" type="text" value="${esc(e.currency ?? "")}">
                </div>
              </div>`;
          }).join("");
          manualQuotaRenderedOnce = true;
        }
      }

      $("btn-save-manual-quota").addEventListener("click", async () => {
        const toast = $("quota-save-toast");
        const providers = {};
        const nowIso = new Date().toISOString();
        ["copilot", "deepseek", "nous", "gemini"].forEach((id) => {
          const remRaw = document.querySelector(`.mq-rem[data-prov="${id}"]`).value.trim();
          const balRaw = document.querySelector(`.mq-bal[data-prov="${id}"]`).value.trim();
          const curRaw = document.querySelector(`.mq-cur[data-prov="${id}"]`).value.trim();
          if (remRaw !== "" || balRaw !== "") {
            const entry = { updated_at: nowIso };
            if (remRaw !== "") entry.percent_remaining = Number(remRaw);
            if (balRaw !== "") {
              entry.balance = Number(balRaw);
              entry.currency = curRaw || "USD";
            }
            providers[id] = entry;
          }
        });
        const resp = await apiFetch("/api/quota", {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ providers }),
        });
        const data = await resp.json().catch(() => ({}));
        if (!resp.ok) {
          toast.className = "toast-banner show err";
          toast.textContent = data.error || "Error al validar quota.json";
          return;
        }
        state.quota = data;
        renderProviderBar();
        renderQuotasView(true);
        toast.className = "toast-banner show ok";
        toast.textContent = `Guardado en ${data.quota_path}`;
      });
