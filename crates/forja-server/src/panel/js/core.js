      const state = {
        view: "resumen",
        sideTab: "runs",
        runs: [],
        agents: [],
        models: null,
        stats: null,
        telemetry: null,
        quota: null,
        policy: { open: ["agy", "mock", "claude", "opencode", "hermes", "local"], sensitive: ["claude"] },
        settings: null,
        selectedRunId: null,
        openPopoverProvider: null,
        sseConnected: false,
        celebrateUntil: 0,
        knownDoneIds: new Set(),
        initializedRuns: false,
        defaultModels: {},
        review: { runId: null, status: "idle", data: null, error: null, actionError: null, pending: null },
      };
      const MODEL_AGENTS = ["claude", "agy", "opencode", "hermes", "local"];
      /* Esfuerzo de razonamiento por familia (el servidor manda los suyos en /api/agents). */
      const EFFORT_LEVELS = {
        claude: ["low", "medium", "high", "xhigh", "max"],
        agy: ["low", "medium", "high", "xhigh", "max"],
        hermes: ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"],
        opencode: ["low", "medium", "high", "max"],
      };
      const EFFORT_SHORT = { none: "Ninguno", minimal: "Mínimo", low: "Bajo", medium: "Med", high: "Alto", xhigh: "Muy alto", max: "Máx", ultra: "Ultra" };
      function effortLevelsOf(agent) {
        const ag = (state.agents || []).find((a) => a.name === agent);
        if (ag && Array.isArray(ag.effort_levels)) return ag.effort_levels;
        return EFFORT_LEVELS[agent] || [];
      }
      function fillEffortSelect(sel, levels, blankLabel, current) {
        sel.textContent = "";
        const b = document.createElement("option");
        b.value = ""; b.textContent = blankLabel;
        sel.appendChild(b);
        const list = levels.slice();
        if (current && !list.includes(current)) list.push(current);
        list.forEach((l) => {
          const o = document.createElement("option");
          o.value = l; o.textContent = EFFORT_SHORT[l] || l;
          sel.appendChild(o);
        });
        sel.value = current || "";
      }
      /* Ganchos que rellena el módulo de terminales */
      const termApi = { onRuns() {}, open() { return false; } };
      const orqHooks = { refresh() {} };
      const orqApi = { reload() {} };
      const filesApi = { onWorkspaceChanged: null, onRuns: null, onViewActive: null };

      const VIEW_META = {
        orquestador: ["Orquestador", "Conversa por texto o por voz; delega en los agentes y revisa sus resultados"],
        archivos: ["Archivos", "Explorador de archivos, pestañas y editor local del proyecto"],
        resumen: ["Resumen", "Estado general del orquestador y actividad en las últimas 24 h"],
        ejecuciones: ["Ejecuciones", "Cola en vivo conectada por Server-Sent Events (/events)"],
        terminales: ["Terminales", "Salida en vivo de varias ejecuciones a la vez"],
        agentes: ["Agentes", "Rendimiento, disponibilidad y carga por adaptador"],
        telemetria: ["Telemetría", "Métricas reales de GPU (nvidia-smi), CPU (/proc/stat) y RAM (/proc/meminfo)"],
        cuotas: ["Cuotas", "Límites por proveedor vía hermes usage y fallback manual quota.json"],
        ajustes: ["Ajustes", "Parámetros operativos, ruta de cuotas y visibilidad de mascota"],
      };

      const $ = (id) => document.getElementById(id);

      function esc(val) {
        return String(val ?? "")
          .replace(/&/g, "&amp;")
          .replace(/</g, "&lt;")
          .replace(/>/g, "&gt;")
          .replace(/"/g, "&quot;");
      }

      function relAgeFromMs(tsMs) {
        if (!tsMs || !Number.isFinite(tsMs)) return "sin dato";
        const diffSec = Math.max(0, Math.floor((Date.now() - tsMs) / 1000));
        if (diffSec < 10) return "ahora";
        if (diffSec < 60) return `${diffSec} s`;
        const mins = Math.floor(diffSec / 60);
        if (mins < 60) return `${mins} min`;
        const hrs = Math.floor(mins / 60);
        if (hrs < 24) return `${hrs} h`;
        return `${Math.floor(hrs / 24)} d`;
      }

      function relAgeFromIso(isoStr) {
        if (!isoStr) return "sin dato";
        const parsed = Date.parse(isoStr);
        if (Number.isNaN(parsed)) return isoStr;
        const age = relAgeFromMs(parsed);
        return age === "ahora" ? "ahora mismo" : `hace ${age}`;
      }

      function fmtDuration(ms) {
        if (ms == null || ms < 0) return "sin dato";
        if (ms < 1000) return `${ms} ms`;
        const s = Math.floor(ms / 1000);
        if (s < 60) return `${s} s`;
        const m = Math.floor(s / 60);
        const rem = s % 60;
        return `${m}m ${rem}s`;
      }

      function runDurationMs(run) {
        if (run.status === "running") {
          return Math.max(0, Date.now() - run.created_at_ms);
        }
        return Math.max(0, (run.updated_at_ms || run.created_at_ms) - run.created_at_ms);
      }

      function levelClass(usedPct) {
        if (usedPct == null || !Number.isFinite(usedPct)) return "";
        if (usedPct > 92) return "crit";
        if (usedPct > 80) return "warn";
        return "ok";
      }

      /* Navegación y barra lateral */
      function setView(view, updateHash = true) {
        if (!VIEW_META[view]) return;
        state.view = view;
        document.querySelectorAll(".nav-item").forEach((b) => {
          b.classList.toggle("active", b.dataset.view === view);
        });
        document.querySelectorAll(".view-pane").forEach((p) => {
          p.classList.toggle("active", p.id === `view-${view}`);
        });
        $("view-heading").textContent = VIEW_META[view][0];
        $("view-subtitle").textContent = VIEW_META[view][1];
        $("sidebar").classList.remove("mobile-open");
        $("sidebar-backdrop").classList.remove("open");
        if (updateHash && window.location.hash !== `#${view}`) {
          history.replaceState(null, "", `#${view}`);
        }
        if (view === "archivos" && filesApi.onViewActive) {
          filesApi.onViewActive();
        }
      }

      function updateSideTabs() {
        $("side-tab-runs").classList.toggle("active", state.sideTab === "runs");
        $("side-tab-agents").classList.toggle("active", state.sideTab === "agents");
        const runsSec = $("side-runs-section");
        const agentsSec = $("side-agents-section");
        if (state.sideTab === "agents") {
          agentsSec.style.order = "-1";
          runsSec.style.order = "1";
        } else {
          runsSec.style.order = "-1";
          agentsSec.style.order = "1";
        }
      }

      /* Renderizado de barra lateral */
      function renderSidebar() {
        const activeCount = state.runs.filter((r) => r.status === "running").length;
        const badge = $("nav-badge-runs");
        badge.textContent = activeCount > 0 ? `${activeCount} vivas` : String(state.runs.length);
        badge.classList.toggle("live", activeCount > 0);

        $("side-agents-count").textContent = String(state.agents.length);
        if (state.agents.length > 0) {
          const byProv = new Map();
          for (const ag of state.agents) {
            if (!byProv.has(ag.provider)) byProv.set(ag.provider, []);
            byProv.get(ag.provider).push(ag);
          }
          let html = "";
          for (const [prov, list] of byProv.entries()) {
            html += `<div class="provider-group-label">${esc(prov)}</div>`;
            for (const ag of list) {
              html += `
                <div class="side-agent-row" data-jump-view="agentes" title="${esc(ag.provider)} · ${esc(ag.model)} (${esc(ag.state)})">
                  <span class="dot ${esc(ag.state)}"></span>
                  <div class="side-row-main">
                    <div class="side-row-title">${esc(ag.name)}</div>
                    <div class="side-row-sub">${esc(ag.model)}</div>
                  </div>
                  <span class="side-counter" title="Tareas activas">${ag.active}</span>
                </div>`;
            }
          }
          $("sidebar-agents-list").innerHTML = html;
        }

        const sortedRuns = [...state.runs].sort((a, b) => b.updated_at_ms - a.updated_at_ms);
        $("side-runs-count").textContent = String(sortedRuns.length);
        if (sortedRuns.length === 0) {
          $("sidebar-runs-list").innerHTML = `<div class="muted" style="font-size:11.5px;padding:4px 2px;">Sin ejecuciones todavía</div>`;
        } else {
          $("sidebar-runs-list").innerHTML = sortedRuns.slice(0, 12).map((r) => `
            <div class="side-run-row" data-open-run="${esc(r.id)}">
              <span class="dot ${esc(r.status)}"></span>
              <div class="side-row-main">
                <div class="side-row-title">${esc(r.task.titulo)}</div>
                <div class="side-row-sub">${esc(r.agent)} · ${esc(relAgeFromMs(r.updated_at_ms))}</div>
              </div>
            </div>
          `).join("");
        }
      }

      /* Mascota reactiva */
      function computeMascotStatus() {
        const active = state.runs.filter((r) => r.status === "running").length;
        const queued = state.runs.filter((r) => r.status === "pending").length;
        const latestTel = state.telemetry && state.telemetry.latest;
        let vramPct = null;
        if (latestTel && latestTel.gpu && latestTel.gpu.memory_total_mib > 0) {
          vramPct = (latestTel.gpu.memory_used_mib / latestTel.gpu.memory_total_mib) * 100;
        }

        const sorted = [...state.runs].sort((a, b) => b.updated_at_ms - a.updated_at_ms);
        const latestFailed = sorted[0] && sorted[0].status === "failed";

        if (Date.now() < state.celebrateUntil) {
          return { mood: "celebrate", label: "Celebrando", text: "¡Ejecución completada!" };
        }
        if (vramPct != null && vramPct > 92) {
          return { mood: "worried", label: "Alerta VRAM", text: `VRAM al ${Math.round(vramPct)} %` };
        }
        if (latestFailed && active === 0) {
          return { mood: "worried", label: "Atención", text: "Última tarea falló" };
        }
        if (active > 0) {
          const suffix = queued > 0 ? ` (+${queued} en cola)` : "";
          return {
            mood: "working",
            label: "Trabajando",
            text: `${active} ${active === 1 ? "tarea" : "tareas"} en curso${suffix}`,
          };
        }
        if (queued > 0) {
          return { mood: "working", label: "En cola", text: `${queued} en espera` };
        }
        return { mood: "idle", label: "En reposo", text: "Todo tranquilo" };
      }

      function renderMascot() {
        const hide = Boolean(state.settings && state.settings.hide_mascot);
        $("mascot-dock").classList.toggle("hidden", hide);
        if (hide) return;
        const m = computeMascotStatus();
        $("mascot-btn").setAttribute("data-mood", m.mood);
        $("mascot-mood-label").textContent = m.label;
        $("mascot-bubble-text").textContent = m.text;
      }

      function syncAgentSelects() {
        if (!state.agents || state.agents.length === 0) return;
        const filterSel = $("filter-agent");
        const prevFilter = filterSel.value || "all";
        filterSel.innerHTML = `<option value="all">Todos los agentes</option>` +
          state.agents.map((ag) => `<option value="${esc(ag.name)}">${esc(ag.name)}</option>`).join("");
        if (Array.from(filterSel.options).some((o) => o.value === prevFilter)) {
          filterSel.value = prevFilter;
        }

        const ntSel = $("nt-agent");
        const prevNt = ntSel.value || "mock";
        ntSel.innerHTML = state.agents.map((ag) =>
          `<option value="${esc(ag.name)}" data-label="${esc(ag.name)} (${esc(ag.provider)})">${esc(ag.name)} (${esc(ag.provider)})</option>`
        ).join("");
        if (Array.from(ntSel.options).some((o) => o.value === prevNt)) {
          ntSel.value = prevNt;
        }
      }

      /* Política en vivo en modal Nueva tarea */
      const BASE_AGENT_LABELS = {
        mock: "mock (local determinista)",
        agy: "agy (Gemini CLI)",
        claude: "claude (Anthropic)",
        opencode: "opencode (OpenCode)",
        hermes: "hermes (Hermes)",
        local: "local (Local)",
      };

      function updateModalPolicyPreview() {
        const priv = $("nt-privacy").value;
        const allowed = (state.policy && state.policy[priv]) || [];
        const banner = $("nt-policy-banner");
        banner.classList.toggle("sensitive", priv === "sensitive");
        const claudeMeta = (state.agents || []).find((a) => a.name === "claude");
        const claudeNote = priv === "sensitive" && allowed.includes("claude") && claudeMeta && !claudeMeta.available
          ? ` <em>(Aviso: claude aún no está configurado en este servidor).</em>`
          : "";
        banner.innerHTML = priv === "sensitive"
          ? `Política <strong>sensitive</strong>: solo se permite <span class="mono">${esc(allowed.join(", "))}</span>. Los agentes externos no autorizados se bloquean antes de crear worktrees.${claudeNote}`
          : `Política <strong>open</strong>: agentes permitidos → <span class="mono">${esc(allowed.join(", "))}</span>.`;

        const sel = $("nt-agent");
        Array.from(sel.options).forEach((opt) => {
          const ok = allowed.includes(opt.value);
          opt.disabled = !ok;
          const base = opt.dataset.label || BASE_AGENT_LABELS[opt.value] || opt.value;
          opt.textContent = ok ? base : `${base} — bloqueado por política`;
        });
        if (!allowed.includes(sel.value) && allowed.length > 0) {
          sel.value = allowed[0];
        }
      }

      function updateModalModelField() {
        const agent = $("nt-agent").value;
        const inp = $("nt-model");
        const note = $("nt-model-note");
        const supported = MODEL_AGENTS.includes(agent);
        inp.disabled = !supported;
        if (!supported) {
          inp.value = "";
          inp.placeholder = "no aplica";
          note.textContent = `El agente ${agent} no admite elegir modelo.`;
        } else {
          const ag = (state.agents || []).find((a) => a.name === agent);
          inp.placeholder = (ag && ag.model) || "por defecto del agente";
          note.textContent = "Vacío = modelo efectivo del agente (Ajustes o default).";
        }
      }

      function updateModalEffortField() {
        const agent = $("nt-agent").value;
        const sel = $("nt-effort");
        const note = $("nt-effort-note");
        const levels = effortLevelsOf(agent);
        const prev = sel.value;
        if (!levels.length) {
          fillEffortSelect(sel, [], "no aplica", "");
          sel.disabled = true;
          note.textContent = `El agente ${agent} no admite esfuerzo de razonamiento.`;
          return;
        }
        sel.disabled = false;
        const ag = (state.agents || []).find((a) => a.name === agent);
        fillEffortSelect(sel, levels, `Esfuerzo de Ajustes (${(ag && ag.effort) || "sin configurar"})`, levels.includes(prev) ? prev : "");
        note.textContent = agent === "opencode"
          ? "Se añade como #variante al modelo (opencode no tiene flag aparte)."
          : "Vacío = el de Ajustes; si no hay ninguno no se pasa ningún flag.";
      }

      /* Acciones HTTP (soporta token Bearer opcional vía sessionStorage o ?token=) */
      const urlToken = new URLSearchParams(window.location.search).get("token");
      if (urlToken) {
        try { sessionStorage.setItem("forja_auth_token", urlToken); } catch (_) {}
      }
      function getAuthToken() {
        try { return sessionStorage.getItem("forja_auth_token") || ""; } catch (_) { return ""; }
      }
      function apiFetch(url, opts = {}) {
        const tok = getAuthToken();
        const headers = Object.assign({}, opts.headers || {});
        if (tok) headers["Authorization"] = `Bearer ${tok}`;
        return fetch(url, Object.assign({}, opts, { headers }));
      }

      async function cancelRun(runId) {
        await apiFetch(`/runs/${encodeURIComponent(runId)}/cancel`, { method: "POST" });
        await refreshCore();
      }

      function upsertRun(record) {
        const prev = state.runs.find((r) => r.id === record.id);
        if (record.status === "done" && (!prev || prev.status !== "done") && state.initializedRuns) {
          state.celebrateUntil = Date.now() + 2600;
        }
        const idx = state.runs.findIndex((r) => r.id === record.id);
        if (idx >= 0) state.runs[idx] = record;
        else state.runs.push(record);
        renderSidebar();
        renderRunsTable();
        renderDrawer();
        renderMascot();
        termApi.onRuns();
        if (filesApi.onRuns) filesApi.onRuns(state.runs);
      }

      async function refreshCore() {
        try {
          const [runsR, agentsR, statsR, policyR, settingsR] = await Promise.all([
            apiFetch("/runs"),
            apiFetch("/api/agents"),
            apiFetch("/api/stats"),
            apiFetch("/api/policy"),
            apiFetch("/api/settings"),
          ]);
          if (runsR.status === 401) {
            $("conn-dot").className = "dot error";
            $("conn-text").textContent = "Token requerido (401)";
            return;
          }
          if (runsR.ok) {
            state.runs = await runsR.json();
            state.initializedRuns = true;
          }
          if (agentsR.ok) {
            state.agents = await agentsR.json();
            syncAgentSelects();
          }
          if (statsR.ok) state.stats = await statsR.json();
          if (policyR.ok) state.policy = await policyR.json();
          if (settingsR.ok) {
            state.settings = await settingsR.json();
            fillSettingsForm(false);
          }
          updateDefaultModels();
          renderSidebar();
          renderSummary();
          renderRunsTable();
          renderAgentsView();
          renderDrawer();
          renderMascot();
          updateModalPolicyPreview();
          updateModalModelField();
          updateModalEffortField();
          orqHooks.refresh();
          termApi.onRuns();
          if (filesApi.onRuns) filesApi.onRuns(state.runs);
        } catch (_) {
          $("conn-dot").className = "dot error";
          $("conn-text").textContent = "Sin conexión";
        }
      }

      async function refreshTelemetry() {
        try {
          const r = await apiFetch("/api/telemetry");
          if (r.ok) {
            state.telemetry = await r.json();
            renderTelemetry();
            renderSummary();
            renderMascot();
          }
        } catch (_) {}
      }

      async function refreshQuota(forceForm) {
        try {
          const r = await apiFetch("/api/quota");
          if (r.ok) {
            state.quota = await r.json();
            renderProviderBar();
            renderQuotasView(Boolean(forceForm));
          }
        } catch (_) {}
      }

      /* SSE /events con reconexión automática */
      let es = null;
      let retryMs = 1000;
      function connectSse() {
        if (es) { try { es.close(); } catch (_) {} }
        const tok = getAuthToken();
        const sseUrl = tok ? `/events?token=${encodeURIComponent(tok)}` : "/events";
        es = new EventSource(sseUrl);
        es.onopen = () => {
          retryMs = 1000;
          state.sseConnected = true;
          $("conn-dot").className = "dot ok";
          $("conn-text").textContent = "Conectado · SSE";
        };
        es.addEventListener("run", (ev) => {
          try {
            const rec = JSON.parse(ev.data);
            upsertRun(rec);
            apiFetch("/api/stats").then((r) => r.ok ? r.json() : null).then((st) => {
              if (st) { state.stats = st; renderSummary(); }
            });
            apiFetch("/api/agents").then((r) => r.ok ? r.json() : null).then((ag) => {
              if (ag) { state.agents = ag; renderSidebar(); renderAgentsView(); }
            });
          } catch (_) {}
        });
        es.onerror = () => {
          state.sseConnected = false;
          $("conn-dot").className = "dot pending";
          $("conn-text").textContent = "Reconectando…";
          try { es.close(); } catch (_) {}
          setTimeout(connectSse, retryMs);
          retryMs = Math.min(8000, retryMs * 2);
        };
      }

      /* Eventos de interfaz */
      document.querySelectorAll(".nav-item").forEach((btn) => {
        btn.addEventListener("click", () => setView(btn.dataset.view));
      });

      document.querySelectorAll(".sidebar-tab").forEach((tab) => {
        tab.addEventListener("click", () => {
          state.sideTab = tab.dataset.sideTab;
          updateSideTabs();
        });
      });

      $("btn-collapse-sidebar").addEventListener("click", () => {
        $("app-shell").classList.toggle("sidebar-collapsed");
      });

      $("btn-mobile-menu").addEventListener("click", () => {
        $("sidebar").classList.toggle("mobile-open");
        $("sidebar-backdrop").classList.toggle("open");
      });
      $("sidebar-backdrop").addEventListener("click", () => {
        $("sidebar").classList.remove("mobile-open");
        $("sidebar-backdrop").classList.remove("open");
      });

      $("mascot-btn").addEventListener("click", () => {
        renderMascot();
        const speech = $("mascot-speech");
        const collapsed = $("app-shell").classList.contains("sidebar-collapsed");
        if (collapsed) {
          speech.classList.remove("bubble-hidden");
          speech.classList.toggle("bubble-floating");
        } else {
          speech.classList.remove("bubble-floating");
          speech.classList.toggle("bubble-hidden");
        }
      });

      document.body.addEventListener("click", (e) => {
        const cancelBtn = e.target.closest("[data-cancel-run]");
        if (cancelBtn) {
          e.stopPropagation();
          cancelRun(cancelBtn.dataset.cancelRun);
          return;
        }
        const runRow = e.target.closest("[data-open-run]");
        if (runRow) {
          state.selectedRunId = runRow.dataset.openRun;
          renderDrawer();
          return;
        }
        const jump = e.target.closest("[data-jump-view]");
        if (jump) {
          setView(jump.dataset.jumpView);
          return;
        }
        const chip = e.target.closest(".provider-chip[data-provider]");
        if (chip) {
          const id = chip.dataset.provider;
          if (state.openPopoverProvider === id) {
            state.openPopoverProvider = null;
            $("provider-popover").classList.remove("open");
          } else {
            state.openPopoverProvider = id;
            const rect = chip.getBoundingClientRect();
            const pop = $("provider-popover");
            pop.style.left = `${Math.max(8, Math.min(window.innerWidth - 372, rect.left))}px`;
            renderProviderPopover(id);
          }
          return;
        }
        if (!e.target.closest("#provider-popover")) {
          state.openPopoverProvider = null;
          $("provider-popover").classList.remove("open");
        }
      });

      const openModal = () => {
        $("nt-error").classList.remove("show");
        updateModalPolicyPreview();
        updateModalModelField();
        updateModalEffortField();
        $("new-task-modal").classList.add("open");
      };
      const closeModal = () => $("new-task-modal").classList.remove("open");

      $("btn-open-new-task").addEventListener("click", openModal);
      $("btn-new-task-runs").addEventListener("click", openModal);
      $("modal-close-btn").addEventListener("click", closeModal);
      $("modal-cancel-btn").addEventListener("click", closeModal);
      $("nt-privacy").addEventListener("change", () => { updateModalPolicyPreview(); updateModalModelField(); updateModalEffortField(); });
      $("nt-agent").addEventListener("change", () => { updateModalModelField(); updateModalEffortField(); });
      updateModalEffortField();

      $("new-task-form").addEventListener("submit", async (e) => {
        e.preventDefault();
        const errBox = $("nt-error");
        errBox.classList.remove("show");
        const payload = {
          titulo: $("nt-title").value.trim(),
          prompt: $("nt-prompt").value.trim(),
          privacidad: $("nt-privacy").value,
          agent: $("nt-agent").value,
        };
        const modelo = $("nt-model").disabled ? "" : $("nt-model").value.trim();
        if (modelo) payload.modelo = modelo;
        const esfuerzo = $("nt-effort").disabled ? "" : $("nt-effort").value;
        if (esfuerzo) payload.esfuerzo = esfuerzo;
        const resp = await apiFetch("/tasks", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(payload),
        });
        if (!resp.ok) {
          const data = await resp.json().catch(() => ({ error: "Error al crear tarea" }));
          errBox.textContent = data.error || `HTTP ${resp.status}`;
          errBox.classList.add("show");
          return;
        }
        const created = await resp.json();
        $("nt-title").value = "";
        $("nt-prompt").value = "";
        $("nt-model").value = "";
        $("nt-effort").value = "";
        closeModal();
        upsertRun(created);
        if ($("nt-open-term").checked && termApi.open(created.id, false)) setView("terminales");
        else setView("ejecuciones");
        await refreshCore();
      });

      $("btn-refresh-now").addEventListener("click", () => {
        refreshCore();
        refreshTelemetry();
        refreshQuota(false);
      });

      document.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
          closeModal();
          state.selectedRunId = null;
          renderDrawer();
          state.openPopoverProvider = null;
          $("provider-popover").classList.remove("open");
        }
      });

      function applyHashRoute() {
        const raw = (window.location.hash || "").replace(/^#/, "").trim();
        if (!raw) return;
        if (VIEW_META[raw]) {
          setView(raw, false);
        } else if (raw === "nueva") {
          openModal();
        } else if (raw === "nueva-sensitive") {
          $("nt-privacy").value = "sensitive";
          openModal();
        }
      }
      window.addEventListener("hashchange", applyHashRoute);

      function bootPanel() {
        /* Arranque y bucles de muestreo */
        const isSnapshot = window.location.search.includes("snapshot=1");
        updateSideTabs();
        applyHashRoute();
        refreshCore();
        refreshTelemetry();
        refreshQuota(true);

        if (isSnapshot) {
          $("conn-dot").className = "dot ok";
          $("conn-text").textContent = "Conectado · SSE";
        } else {
          connectSse();
          setInterval(refreshTelemetry, 2000);
          setInterval(() => {
            document.querySelectorAll("[data-live-duration]").forEach((el) => {
              const start = Number(el.dataset.liveDuration);
              if (Number.isFinite(start)) {
                el.textContent = fmtDuration(Math.max(0, Date.now() - start));
              }
            });
            renderMascot();
          }, 1000);
          setInterval(refreshCore, 8000);
          setInterval(() => refreshQuota(false), 30000);
        }
      }
