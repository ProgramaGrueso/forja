      /* Terminales en paralelo: salida en vivo por SSE (/runs/{id}/stream) */
      (function terminales() {
        const MAX_LINES = 5000;
        const MAX_PANELS = 4;
        const grid = $("term-grid");
        const countSel = $("term-count");
        const colsSel = $("term-cols");
        const lsGet = (k) => { try { return localStorage.getItem(k); } catch (_) { return null; } };
        const lsSet = (k, v) => { try { localStorage.setItem(k, v); } catch (_) {} };
        const clamp = (n, lo, hi, d) => (Number.isFinite(n) ? Math.max(lo, Math.min(hi, n)) : d);
        let count = clamp(parseInt(lsGet("forja_term_count"), 10), 1, MAX_PANELS, 2);
        let cols = clamp(parseInt(lsGet("forja_term_cols"), 10), 1, MAX_PANELS, 2);
        const panels = [];
        let restored = false;

        function saveLayout() {
          lsSet("forja_term_count", String(count));
          lsSet("forja_term_cols", String(cols));
          lsSet("forja_term_runs", JSON.stringify(panels.slice(0, count).map((p) => p.runId || "")));
        }

        function sysLine(p, text) {
          const d = document.createElement("div");
          d.className = "term-line sys";
          d.textContent = text;
          p.body.appendChild(d);
          scrollEnd(p);
        }

        function scrollEnd(p) {
          if (p.paused || p.raf) return;
          p.raf = requestAnimationFrame(() => { p.raf = 0; p.body.scrollTop = p.body.scrollHeight; });
        }

        function appendLine(p, stream, text) {
          const d = document.createElement("div");
          d.className = "term-line" + (stream === "stderr" ? " err" : "");
          const t = String(text == null ? "" : text).replace(/\r?\n$/, "");
          d.textContent = t === "" ? "\u00a0" : t;   // textContent: nunca innerHTML con datos del servidor
          p.body.appendChild(d);
          p.lines += 1;
          while (p.body.childElementCount > MAX_LINES) { p.body.removeChild(p.body.firstChild); p.dropped = true; }
          scrollEnd(p);
        }

        function runOf(p) { return p.runId ? state.runs.find((r) => r.id === p.runId) : null; }

        function updateHeader(p) {
          const r = runOf(p);
          let label = "sin ejecución";
          let cls = "badge";
          if (p.runId && r) { label = r.status; cls = `badge ${r.status}`; }
          else if (p.runId) { label = "desconocida"; }
          if (p.reconnecting && !p.finished) label += " · reconectando";
          p.badge.className = cls;
          p.badge.textContent = label;
          p.cancel.style.display = r && (r.status === "running" || r.status === "pending") ? "inline-flex" : "none";
          const live = p.es ? "en vivo" : (p.finished ? "terminada" : "—");
          p.meta.textContent = r
            ? `${r.agent}${r.model ? " · " + r.model : ""} · ${r.id.slice(0, 8)} · ${live} · ${p.body.childElementCount} líneas${p.dropped ? " (recortadas al tope)" : ""}`
            : "Elige una ejecución en el selector";
        }

        function fillSelect(p) {
          const runs = [...state.runs].sort((a, b) => b.updated_at_ms - a.updated_at_ms).slice(0, 40);
          const sig = runs.map((r) => `${r.id}:${r.status}`).join("|") + "#" + p.runId;
          if (p.sig === sig || document.activeElement === p.sel) return;
          p.sig = sig;
          p.sel.textContent = "";
          const none = document.createElement("option");
          none.value = ""; none.textContent = "— elegir ejecución —";
          p.sel.appendChild(none);
          let hasCur = !p.runId;
          runs.forEach((r) => {
            const o = document.createElement("option");
            o.value = r.id;
            o.textContent = `${r.status} · ${r.task.titulo} · ${r.id.slice(0, 8)}`;
            if (r.id === p.runId) hasCur = true;
            p.sel.appendChild(o);
          });
          if (!hasCur) {
            const o = document.createElement("option");
            o.value = p.runId; o.textContent = p.runId.slice(0, 8);
            p.sel.appendChild(o);
          }
          p.sel.value = p.runId || "";
        }

        function detach(p) {
          if (p.es) { try { p.es.close(); } catch (_) {} p.es = null; }
          if (p.timer) { clearTimeout(p.timer); p.timer = null; }
          p.reconnecting = false;
        }

        function clearBody(p) {
          p.body.textContent = "";
          p.lines = 0;
          p.dropped = false;
        }

        function attach(p, runId) {
          detach(p);
          clearBody(p);
          p.runId = runId || "";
          p.nextSeq = 0;
          p.finished = false;
          p.retryMs = 1000;
          p.paused = false;
          p.el.classList.remove("paused");
          if (!p.runId) sysLine(p, "Panel libre. Elige una ejecución en el selector.");
          else connect(p);
          p.sig = null;
          fillSelect(p);
          updateHeader(p);
          saveLayout();
        }

        function connect(p) {
          if (!p.runId) return;
          const runId = p.runId;
          const tok = getAuthToken();
          const url = `/runs/${encodeURIComponent(runId)}/stream?since=${p.nextSeq}` + (tok ? `&token=${encodeURIComponent(tok)}` : "");
          const es = new EventSource(url);
          p.es = es;
          es.onopen = () => { p.reconnecting = false; p.retryMs = 1000; updateHeader(p); };
          es.addEventListener("output", (ev) => {
            if (p.runId !== runId) return;
            try {
              const d = JSON.parse(ev.data);
              if (d.seq < p.nextSeq) return;
              appendLine(p, d.stream, d.text);
              p.nextSeq = d.seq + 1;
              if ((p.lines & 15) === 0) updateHeader(p);
            } catch (_) {}
          });
          es.addEventListener("status", (ev) => {
            if (p.runId !== runId) return;
            p.finished = true;
            detach(p);
            try { upsertRun(JSON.parse(ev.data)); } catch (_) {}
            updateHeader(p);
          });
          es.onerror = () => {
            if (p.runId !== runId || p.finished) return;
            try { es.close(); } catch (_) {}
            p.es = null;
            p.reconnecting = true;
            updateHeader(p);
            recover(p, runId);
          };
        }

        /* Tras un corte: comprueba por HTTP si el run existe/terminó y reconecta con ?since=seq */
        async function recover(p, runId) {
          try {
            const r = await apiFetch(`/runs/${encodeURIComponent(runId)}/output?since=${p.nextSeq}`);
            if (p.runId !== runId) return;
            if (r.status === 404) { p.reconnecting = false; sysLine(p, "La ejecución ya no existe."); updateHeader(p); return; }
            if (r.ok) {
              const d = await r.json();
              if (p.runId !== runId) return;
              (d.chunks || []).forEach((c) => { if (c.seq >= p.nextSeq) { appendLine(p, c.stream, c.text); p.nextSeq = c.seq + 1; } });
              if (d.finished) { p.finished = true; p.reconnecting = false; updateHeader(p); refreshCore(); return; }
            }
          } catch (_) { /* sin red: reintenta */ }
          if (p.runId !== runId || p.finished) return;
          p.timer = setTimeout(() => { p.timer = null; if (p.runId === runId && !p.finished) connect(p); }, p.retryMs);
          p.retryMs = Math.min(8000, p.retryMs * 2);
        }

        function makePanel(i) {
          const el = document.createElement("div");
          el.className = "term-panel";
          const head = document.createElement("div");
          head.className = "term-head";
          const sel = document.createElement("select");
          sel.className = "select";
          sel.setAttribute("aria-label", `Ejecución del panel ${i + 1}`);
          const badge = document.createElement("span");
          badge.className = "badge";
          const clear = document.createElement("button");
          clear.type = "button"; clear.className = "btn"; clear.textContent = "Limpiar";
          clear.setAttribute("aria-label", `Limpiar panel ${i + 1}`);
          const cancel = document.createElement("button");
          cancel.type = "button"; cancel.className = "btn btn-danger"; cancel.textContent = "Cancelar";
          cancel.setAttribute("aria-label", `Cancelar la ejecución del panel ${i + 1}`);
          const close = document.createElement("button");
          close.type = "button"; close.className = "btn"; close.textContent = "Cerrar";
          close.setAttribute("aria-label", `Cerrar el panel ${i + 1}`);
          const meta = document.createElement("div");
          meta.className = "term-meta";
          head.append(sel, badge, clear, cancel, close, meta);
          const wrap = document.createElement("div");
          wrap.className = "term-wrap";
          const body = document.createElement("div");
          body.className = "term-body";
          body.tabIndex = 0;
          body.setAttribute("role", "log");
          body.setAttribute("aria-label", `Salida del panel ${i + 1}`);
          const pause = document.createElement("button");
          pause.type = "button"; pause.className = "btn term-pause"; pause.textContent = "↓ En pausa · ir al final";
          wrap.append(body, pause);
          el.append(head, wrap);
          const p = { i, el, sel, badge, clear, cancel, close, meta, body, pause, runId: "", es: null, timer: null, nextSeq: 0, lines: 0, dropped: false, paused: false, finished: false, reconnecting: false, retryMs: 1000, raf: 0, sig: null };
          sel.addEventListener("change", () => attach(p, sel.value));
          clear.addEventListener("click", () => { clearBody(p); updateHeader(p); });
          close.addEventListener("click", () => attach(p, ""));
          cancel.addEventListener("click", () => { if (p.runId) cancelRun(p.runId); });
          body.addEventListener("scroll", () => {
            p.paused = body.scrollHeight - body.scrollTop - body.clientHeight > 24;
            el.classList.toggle("paused", p.paused);
          });
          pause.addEventListener("click", () => { p.paused = false; el.classList.remove("paused"); body.scrollTop = body.scrollHeight; });
          sysLine(p, "Panel libre. Elige una ejecución en el selector.");
          return p;
        }

        function layout() {
          while (panels.length < count) panels.push(makePanel(panels.length));
          while (panels.length > count) { const p = panels.pop(); detach(p); p.el.remove(); }
          panels.forEach((p) => { if (!p.el.isConnected) grid.appendChild(p.el); });
          const effCols = Math.min(cols, count);
          grid.style.setProperty("--term-cols", String(effCols));
          grid.style.setProperty("--term-rows", String(Math.ceil(count / effCols)));
          countSel.value = String(count);
          colsSel.value = String(cols);
          panels.forEach((p) => { fillSelect(p); updateHeader(p); });
          saveLayout();
        }

        function restore() {
          if (restored || !state.initializedRuns) return;
          restored = true;
          let saved = [];
          try { saved = JSON.parse(lsGet("forja_term_runs") || "[]"); } catch (_) {}
          saved.slice(0, count).forEach((id, i) => {
            if (id && panels[i] && state.runs.some((r) => r.id === id)) attach(panels[i], id);
          });
        }

        termApi.onRuns = () => {
          restore();
          panels.forEach((p) => { fillSelect(p); updateHeader(p); });
        };

        /* Abre una ejecución: panel que ya la muestra > primer panel libre > (si force) el último panel */
        termApi.open = (runId, force) => {
          const existing = panels.find((p) => p.runId === runId);
          if (existing) return true;
          let free = panels.find((p) => !p.runId);
          if (!free && force) free = panels[panels.length - 1];
          if (!free) return false;
          attach(free, runId);
          return true;
        };

        countSel.addEventListener("change", () => { count = clamp(parseInt(countSel.value, 10), 1, MAX_PANELS, 2); layout(); });
        colsSel.addEventListener("change", () => { cols = clamp(parseInt(colsSel.value, 10), 1, MAX_PANELS, 2); layout(); });
        window.addEventListener("beforeunload", () => panels.forEach(detach));
        layout();
      })();
