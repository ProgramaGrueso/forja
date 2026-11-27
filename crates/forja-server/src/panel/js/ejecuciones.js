      /* Vista Ejecuciones + Drawer */
      function renderRunsTable() {
        const stFilter = $("filter-status").value;
        const agFilter = $("filter-agent").value;
        const q = $("filter-query").value.trim().toLowerCase();

        const filtered = [...state.runs]
          .sort((a, b) => b.created_at_ms - a.created_at_ms)
          .filter((r) => stFilter === "all" || r.status === stFilter)
          .filter((r) => agFilter === "all" || r.agent === agFilter)
          .filter((r) => !q || r.task.titulo.toLowerCase().includes(q) || r.id.toLowerCase().includes(q));

        if (filtered.length === 0) {
          $("runs-tbody").innerHTML = `<tr><td colspan="7" class="muted">No hay ejecuciones que coincidan con el filtro.</td></tr>`;
          return;
        }

        $("runs-tbody").innerHTML = filtered.map((r) => {
          const canCancel = r.status === "running" || r.status === "pending";
          return `
            <tr data-open-run="${esc(r.id)}">
              <td><span class="badge ${esc(r.status)}">${esc(r.status)}</span></td>
              <td>
                <div style="font-weight:600;">${esc(r.task.titulo)}</div>
                <div class="mono muted" style="font-size:11px;">${esc(r.id.slice(0, 8))}</div>
              </td>
              <td class="mono">${esc(r.agent)}</td>
              <td class="mono" ${r.status === "running" ? `data-live-duration="${r.created_at_ms}"` : ""}>${esc(fmtDuration(runDurationMs(r)))}</td>
              <td class="mono">${r.attempts}</td>
              <td class="mono">${esc(r.task.privacidad)}</td>
              <td>
                ${canCancel ? `<button class="btn btn-danger" style="padding:3px 8px;font-size:11px;" data-cancel-run="${esc(r.id)}" type="button">Cancelar</button>` : `<span class="muted">—</span>`}
              </td>
            </tr>`;
        }).join("");
      }

      function renderDrawer() {
        const drawer = $("detail-drawer");
        if (!state.selectedRunId) {
          drawer.classList.remove("open");
          return;
        }
        const r = state.runs.find((x) => x.id === state.selectedRunId);
        if (!r) {
          drawer.classList.remove("open");
          return;
        }
        drawer.classList.add("open");
        $("drawer-title").textContent = r.task.titulo;
        $("drawer-id").textContent = r.id;
        const canCancel = r.status === "running" || r.status === "pending";
        $("drawer-cancel-btn").style.display = canCancel ? "inline-flex" : "none";
        $("drawer-cancel-btn").onclick = () => cancelRun(r.id);
        $("drawer-term-btn").onclick = () => {
          termApi.open(r.id, true);
          setView("terminales");
        };
        renderDrawerRetry(r);

        $("drawer-meta").innerHTML = `
          <span class="muted">Estado:</span><span><span class="badge ${esc(r.status)}">${esc(r.status)}</span></span>
          <span class="muted">Agente:</span><span class="mono">${esc(r.agent)}</span>
          <span class="muted">Modelo:</span><span class="mono">${esc(r.model || "sin modelo")}</span>
          <span class="muted">Esfuerzo:</span><span class="mono">${esc(r.effort || "sin configurar")}</span>
          <span class="muted">Privacidad:</span><span class="mono">${esc(r.task.privacidad)}</span>
          <span class="muted">Duración:</span><span class="mono" ${r.status === "running" ? `data-live-duration="${r.created_at_ms}"` : ""}>${esc(fmtDuration(runDurationMs(r)))}</span>
          <span class="muted">Intentos:</span><span class="mono">${r.attempts}</span>
          <span class="muted">Rama:</span><span class="mono">${esc(r.branch || "sin rama")}</span>
          <span class="muted">Worktree:</span><span class="mono">${esc(r.worktree || "sin worktree")}</span>
        `;
        $("drawer-prompt").textContent = r.task.prompt || "";
        if (r.error) {
          $("drawer-output-label").textContent = "Error";
          $("drawer-output").textContent = r.error;
        } else {
          $("drawer-output-label").textContent = "Salida";
          $("drawer-output").textContent = r.output || (r.status === "running" ? "Ejecutando…" : "Sin salida registrada.");
        }
        renderDrawerReview();
        ensureReview(r);
      }

      /* Reintento: relanza una ejecución cerrada con los mismos datos. */
      function renderDrawerRetry(r) {
        const btn = $("drawer-retry-btn");
        const retryable = ["done", "failed", "cancelled"].includes(r.status);
        const pending = state.retry.pending.has(r.id);
        btn.style.display = retryable ? "inline-flex" : "none";
        btn.disabled = pending;
        btn.textContent = pending ? "Reintentando…" : "Reintentar";
        btn.onclick = () => retryRun(r.id);
        const failure = retryable ? state.retry.errors.get(r.id) : null;
        const recreate = $("drawer-retry-recreate");
        $("drawer-retry-box").style.display = failure ? "block" : "none";
        $("drawer-retry-error").textContent = failure ? failure.message : "";
        recreate.style.display = failure && failure.recreate ? "inline-flex" : "none";
        recreate.onclick = () => openTaskFromRun(r);
      }

      async function retryRun(runId) {
        if (state.retry.pending.has(runId)) return;
        state.retry.pending.add(runId);
        state.retry.errors.delete(runId);
        renderDrawer();
        let created = null;
        try {
          const resp = await apiFetch(`/runs/${encodeURIComponent(runId)}/retry`, { method: "POST" });
          const data = await resp.json().catch(() => ({}));
          if (resp.status === 201 && data.id) {
            created = data;
          } else {
            const run = state.runs.find((x) => x.id === runId);
            const recreate = resp.status === 409 && Boolean(run && run.prompt_redacted);
            const reason = data.error || `HTTP ${resp.status}`;
            state.retry.errors.set(runId, {
              recreate,
              message: recreate
                ? `El prompt se guardó con secretos redactados y no se puede relanzar tal cual: crea la tarea de nuevo y escribe el prompt completo. (${reason})`
                : `No se pudo reintentar: ${reason}`,
            });
          }
        } catch (_) {
          state.retry.errors.set(runId, {
            recreate: false,
            message: "No se pudo contactar con el servidor. Revisa la lista antes de volver a intentarlo.",
          });
        } finally {
          state.retry.pending.delete(runId);
        }
        if (created) {
          state.selectedRunId = created.id;
          // El evento en vivo puede haber llegado antes con un estado más nuevo.
          if (state.runs.some((x) => x.id === created.id)) renderDrawer();
          else upsertRun(created);
        } else {
          renderDrawer();
        }
        await refreshCore();
      }

      /* Abre «Nueva tarea» con los datos de una ejecución, sin su prompt redactado. */
      function openTaskFromRun(r) {
        $("nt-title").value = r.task.titulo || "";
        $("nt-prompt").value = "";
        $("nt-privacy").value = r.task.privacidad;
        $("nt-agent").value = r.agent;
        openModal();
        if ($("nt-agent").value === r.agent) {
          if (!$("nt-model").disabled) $("nt-model").value = r.model || "";
          const effort = $("nt-effort");
          effort.value = Array.from(effort.options).some((o) => o.value === r.effort) ? r.effort : "";
        } else {
          $("nt-model").value = "";
          $("nt-effort").value = "";
          $("nt-error").textContent = `El agente ${r.agent} no está permitido para esta privacidad; revisa agente, modelo y esfuerzo.`;
          $("nt-error").classList.add("show");
        }
        $("nt-prompt").focus();
      }

      /* Revisión del worktree: diff, fusión y descarte desde el detalle. */
      function emptyReview() {
        return { runId: null, status: "idle", data: null, error: null, actionError: null, pending: null };
      }

      function isReviewable(r) {
        return Boolean(r && r.branch && ["done", "failed", "cancelled"].includes(r.status));
      }

      function renderDrawerReview() {
        const section = $("drawer-review");
        const r = state.runs.find((x) => x.id === state.selectedRunId);
        if (!r || !isReviewable(r)) {
          section.style.display = "none";
          return;
        }
        section.style.display = "block";
        const rev = state.review.runId === r.id
          ? state.review
          : Object.assign(emptyReview(), { runId: r.id, status: "loading" });
        const status = $("drawer-review-status");
        const actions = $("drawer-review-actions");
        const retry = $("drawer-review-retry");
        const statBox = $("drawer-review-stat");
        const diffBox = $("drawer-diff");
        status.style.color = "";
        retry.style.display = "none";

        if (rev.status === "loading") {
          status.textContent = "Calculando diff…";
          actions.style.display = "none";
          statBox.style.display = "none";
          diffBox.style.display = "none";
          return;
        }
        if (rev.status === "error") {
          status.textContent = rev.error || "No se pudo cargar la revisión.";
          status.style.color = "#fca5a5";
          actions.style.display = "none";
          retry.style.display = "inline-flex";
          retry.onclick = () => {
            state.review = emptyReview();
            renderDrawer();
          };
          statBox.style.display = "none";
          diffBox.style.display = "none";
          return;
        }
        if (rev.status === "closed") {
          const d = rev.data || {};
          if (d.closed) {
            status.textContent = d.message || "El ciclo de revisión de esta ejecución ya está cerrado.";
            status.style.color = "#93c5fd";
          } else {
            status.textContent = d.merged
              ? `Fusionado en ${d.base || "la rama base"}${d.commit ? ` (${d.commit})` : ""} y worktree retirado.`
              : "Trabajo descartado y worktree limpiado.";
            if (d.worktree_commit) status.textContent += ` Se commiteó antes el trabajo pendiente (${d.worktree_commit}).`;
            if (d.warning) status.textContent += ` Aviso: ${d.warning}`;
            status.style.color = d.warning ? "#fbbf24" : "#a7f3d0";
          }
          actions.style.display = "none";
          statBox.style.display = "none";
          diffBox.style.display = "none";
          return;
        }

        const d = rev.data || {};
        if (rev.pending) {
          status.textContent = rev.pending === "merge" ? "Fusionando…" : "Descartando…";
        } else if (rev.actionError) {
          status.textContent = rev.actionError;
          status.style.color = "#fca5a5";
        } else {
          status.textContent = `${d.branch || r.branch} sobre ${d.base || "la rama base"}${d.truncated ? " · diff truncado" : ""}`;
        }
        actions.style.display = "flex";
        statBox.textContent = d.stat || "Sin cambios respecto a la rama base.";
        statBox.style.display = "block";
        diffBox.textContent = d.diff || "Sin diferencias.";
        diffBox.style.display = "block";
        const mergeBtn = $("drawer-merge-btn");
        const discardBtn = $("drawer-discard-btn");
        mergeBtn.disabled = Boolean(rev.pending) || r.status !== "done";
        mergeBtn.title = r.status !== "done"
          ? "Solo se fusionan ejecuciones completadas con éxito; usa descartar para el resto."
          : "";
        discardBtn.disabled = Boolean(rev.pending);
        mergeBtn.onclick = () => reviewAction(r.id, "merge");
        discardBtn.onclick = () => reviewAction(r.id, "discard");
      }

      async function ensureReview(r) {
        if (!isReviewable(r)) {
          if (state.review.runId !== null) state.review = emptyReview();
          return;
        }
        if (state.review.runId === r.id && state.review.status !== "idle") return;
        state.review = Object.assign(emptyReview(), { runId: r.id, status: "loading" });
        renderDrawerReview();
        try {
          const resp = await apiFetch(`/runs/${encodeURIComponent(r.id)}/diff`);
          const data = await resp.json().catch(() => ({}));
          if (state.selectedRunId !== r.id) {
            if (state.review.runId === r.id) state.review = emptyReview();
            return;
          }
          const active = String(data.error || "").includes("sigue activa");
          if (resp.status === 409 && !active) {
            // El worktree ya no existe: el ciclo se cerró desde otra pestaña o
            // antes de recargar. Una ejecución aún activa es un error con reintento.
            state.review = Object.assign(emptyReview(), {
              runId: r.id,
              status: "closed",
              data: { closed: true, message: data.error },
            });
          } else if (!resp.ok) {
            state.review = Object.assign(emptyReview(), {
              runId: r.id,
              status: "error",
              error: data.error || `HTTP ${resp.status}`,
            });
          } else {
            state.review = Object.assign(emptyReview(), { runId: r.id, status: "ready", data });
          }
        } catch (_) {
          if (state.selectedRunId !== r.id) {
            if (state.review.runId === r.id) state.review = emptyReview();
            return;
          }
          state.review = Object.assign(emptyReview(), {
            runId: r.id,
            status: "error",
            error: "No se pudo cargar el diff (¿servidor desconectado?)",
          });
        }
        renderDrawerReview();
      }

      async function reviewAction(runId, action) {
        const isMerge = action === "merge";
        if (state.review.runId !== runId || state.review.pending) return;
        const message = isMerge
          ? "¿Fusionar la rama de esta ejecución en la rama base y retirar el worktree? El repositorio base debe estar limpio y en su rama; los cambios pendientes del worktree se commitean antes."
          : "¿Descartar todo el trabajo de esta ejecución (cambios sin commitear y commits de su rama) y limpiar el worktree? No se puede deshacer.";
        if (!window.confirm(message)) return;
        state.review = Object.assign({}, state.review, { pending: action, actionError: null });
        renderDrawerReview();
        let resp;
        try {
          resp = await apiFetch(`/runs/${encodeURIComponent(runId)}/${action}`, { method: "POST" });
        } catch (_) {
          state.review = Object.assign({}, state.review, {
            pending: null,
            actionError: "No se pudo contactar con el servidor.",
          });
          if (state.selectedRunId === runId) renderDrawerReview();
          return;
        }
        const data = await resp.json().catch(() => ({}));
        if (state.selectedRunId !== runId) {
          if (state.review.runId === runId) state.review = emptyReview();
          return;
        }
        if (!resp.ok) {
          state.review = Object.assign({}, state.review, {
            pending: null,
            actionError: data.error || `HTTP ${resp.status}`,
          });
          renderDrawerReview();
          return;
        }
        state.review = Object.assign(emptyReview(), { runId, status: "closed", data });
        renderDrawerReview();
        await refreshCore();
      }

      $("drawer-close-btn").addEventListener("click", () => {
        state.selectedRunId = null;
        renderDrawer();
      });

      ["filter-status", "filter-agent", "filter-query"].forEach((id) => {
        $(id).addEventListener("input", renderRunsTable);
      });
