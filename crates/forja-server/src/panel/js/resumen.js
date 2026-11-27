      function renderTimelineChart() {
        const svg = $("chart-timeline-svg");
        const buckets = (state.stats && state.stats.timeline) || [];
        if (buckets.length === 0) {
          svg.innerHTML = `<text x="300" y="78" text-anchor="middle" fill="#596275" font-size="12">Sin actividad en las últimas 24 h</text>`;
          return;
        }
        const W = 600, H = 150, padL = 28, padR = 10, padT = 14, padB = 22;
        const plotW = W - padL - padR;
        const plotH = H - padT - padB;
        const maxVal = Math.max(1, ...buckets.map((b) => b.done + b.failed + b.other));
        const slot = plotW / buckets.length;
        const barW = Math.max(6, slot * 0.62);

        let out = `<line x1="${padL}" y1="${padT + plotH}" x2="${W - padR}" y2="${padT + plotH}" stroke="#242f45" stroke-width="1"/>`;
        out += `<text x="${padL - 5}" y="${padT + 6}" text-anchor="end" fill="#596275" font-size="9.5" font-family="monospace">${maxVal}</text>`;
        out += `<text x="${padL - 5}" y="${padT + plotH}" text-anchor="end" fill="#596275" font-size="9.5" font-family="monospace">0</text>`;

        buckets.forEach((b, i) => {
          const x = padL + i * slot + (slot - barW) / 2;
          let yCursor = padT + plotH;
          const segs = [
            [b.done, "#10b981"],
            [b.failed, "#ef4444"],
            [b.other, "#8b5cf6"],
          ];
          for (const [val, color] of segs) {
            if (!val) continue;
            const h = (val / maxVal) * plotH;
            yCursor -= h;
            out += `<rect x="${x.toFixed(1)}" y="${yCursor.toFixed(1)}" width="${barW.toFixed(1)}" height="${h.toFixed(1)}" fill="${color}" rx="2"/>`;
          }
          if (i % 6 === 0 || i === buckets.length - 1) {
            const dt = new Date(b.start_ms);
            const hh = `${String(dt.getHours()).padStart(2, "0")}h`;
            out += `<text x="${(x + barW / 2).toFixed(1)}" y="${H - 6}" text-anchor="middle" fill="#596275" font-size="9.5" font-family="monospace">${hh}</text>`;
          }
        });
        svg.innerHTML = out;
      }

      /* Vista Resumen */
      function renderSummary() {
        if (state.stats) {
          $("kpi-active").textContent = String(state.stats.active);
          $("kpi-queued").textContent = String(state.stats.queued);
          $("kpi-done24").textContent = String(state.stats.done_24h);
          $("kpi-failed24").textContent = String(state.stats.failed_24h);
          $("forja-version").textContent = `Forja v${state.stats.version}`;
        }
        renderTimelineChart();

        const recent = [...state.runs].sort((a, b) => b.updated_at_ms - a.updated_at_ms).slice(0, 8);
        if (recent.length === 0) {
          $("summary-recent-tbody").innerHTML = `<tr><td colspan="6" class="muted">Sin ejecuciones registradas.</td></tr>`;
        } else {
          $("summary-recent-tbody").innerHTML = recent.map((r) => `
            <tr data-open-run="${esc(r.id)}">
              <td><span class="badge ${esc(r.status)}">${esc(r.status)}</span></td>
              <td><strong>${esc(r.task.titulo)}</strong></td>
              <td class="mono">${esc(r.agent)}</td>
              <td class="mono">${esc(r.task.privacidad)}</td>
              <td class="mono">${esc(fmtDuration(runDurationMs(r)))}</td>
              <td class="muted">${esc(relAgeFromMs(r.updated_at_ms))}</td>
            </tr>
          `).join("");
        }

        const latest = state.telemetry && state.telemetry.latest;
        if (latest) {
          const gpuLine = latest.gpu
            ? `${esc(latest.gpu.name)} · VRAM ${latest.gpu.memory_used_mib}/${latest.gpu.memory_total_mib} MiB (${Math.round((latest.gpu.memory_used_mib / Math.max(1, latest.gpu.memory_total_mib)) * 100)}%) · ${latest.gpu.temperature_c.toFixed(0)} °C`
            : esc(latest.gpu_error || "GPU no disponible");
          const cpuStr = latest.cpu_percent != null ? `${latest.cpu_percent.toFixed(1)}%` : "calculando delta…";
          const ramStr = (latest.ram_used_kib != null && latest.ram_total_kib)
            ? `${(latest.ram_used_kib / 1048576).toFixed(1)} / ${(latest.ram_total_kib / 1048576).toFixed(1)} GiB`
            : "sin dato";
          $("summary-hw-box").innerHTML = `
            <div style="font-size:12.5px;"><strong>GPU:</strong> <span class="mono">${gpuLine}</span></div>
            <div style="font-size:12.5px;"><strong>CPU:</strong> <span class="mono">${esc(cpuStr)}</span> &nbsp;·&nbsp; <strong>RAM:</strong> <span class="mono">${esc(ramStr)}</span></div>
          `;
        }
      }
