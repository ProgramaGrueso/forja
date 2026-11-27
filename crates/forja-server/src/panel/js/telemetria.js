      /* Gráficos SVG puros */
      function renderLineSvg(svgEl, seriesList, showThresholds = false) {
        const vb = svgEl.viewBox && svgEl.viewBox.baseVal;
        const W = (vb && vb.width) ? vb.width : 600;
        const H = (vb && vb.height) ? vb.height : 150;
        const padL = 34, padR = 10, padT = 12, padB = 20;
        const plotW = W - padL - padR;
        const plotH = H - padT - padB;
        let grid = "";
        for (const pct of [0, 25, 50, 75, 100]) {
          const y = padT + plotH - (pct / 100) * plotH;
          grid += `<line x1="${padL}" y1="${y.toFixed(1)}" x2="${W - padR}" y2="${y.toFixed(1)}" stroke="#192030" stroke-width="1"/>`;
          grid += `<text x="${padL - 5}" y="${(y + 3).toFixed(1)}" text-anchor="end" fill="#596275" font-size="9.5" font-family="monospace">${pct}</text>`;
        }
        if (showThresholds) {
          const y80 = padT + plotH - 0.8 * plotH;
          const y92 = padT + plotH - 0.92 * plotH;
          grid += `<line x1="${padL}" y1="${y80.toFixed(1)}" x2="${W - padR}" y2="${y80.toFixed(1)}" stroke="rgba(245,158,11,0.45)" stroke-dasharray="4 4" stroke-width="1"/>`;
          grid += `<line x1="${padL}" y1="${y92.toFixed(1)}" x2="${W - padR}" y2="${y92.toFixed(1)}" stroke="rgba(239,68,68,0.5)" stroke-dasharray="4 4" stroke-width="1"/>`;
        }

        let paths = "";
        let hasPoints = false;
        for (const s of seriesList) {
          const pts = s.values.filter((v) => v != null && Number.isFinite(v));
          if (pts.length === 0) continue;
          hasPoints = true;
          const n = s.values.length;
          const coords = [];
          s.values.forEach((v, i) => {
            if (v == null || !Number.isFinite(v)) return;
            const x = padL + (n <= 1 ? plotW : (i / (n - 1)) * plotW);
            const clamped = Math.max(0, Math.min(100, v));
            const y = padT + plotH - (clamped / 100) * plotH;
            coords.push([x, y]);
          });
          if (coords.length === 1) {
            coords.push([W - padR, coords[0][1]]);
          }
          const d = coords.map((c, idx) => `${idx === 0 ? "M" : "L"}${c[0].toFixed(1)},${c[1].toFixed(1)}`).join(" ");
          if (s.fill) {
            const area = `${d} L${coords[coords.length - 1][0].toFixed(1)},${(padT + plotH).toFixed(1)} L${coords[0][0].toFixed(1)},${(padT + plotH).toFixed(1)} Z`;
            paths += `<path d="${area}" fill="${s.fill}"/>`;
          }
          paths += `<path d="${d}" fill="none" stroke="${s.stroke}" stroke-width="2"/>`;
        }

        if (!hasPoints) {
          paths = `<text x="${W / 2}" y="${H / 2}" text-anchor="middle" fill="#596275" font-size="12">Sin muestras disponibles</text>`;
        }
        svgEl.innerHTML = grid + paths;
      }

      /* Vista Telemetría */
      function renderTelemetry() {
        const snap = state.telemetry;
        if (!snap || !snap.latest) return;
        const cur = snap.latest;
        const hist = snap.history || [];

        if (cur.gpu) {
          const g = cur.gpu;
          const vramPct = g.memory_total_mib > 0 ? (g.memory_used_mib / g.memory_total_mib) * 100 : 0;
          $("tel-gpu-name").textContent = g.name;
          $("tel-vram-value").textContent = `${g.memory_used_mib} / ${g.memory_total_mib} MiB (${vramPct.toFixed(1)}%)`;
          const bar = $("tel-vram-bar");
          bar.style.width = `${Math.min(100, vramPct).toFixed(1)}%`;
          bar.className = `meter-fill ${levelClass(vramPct)}`;
          $("tel-gpu-extra").textContent = `Utilización GPU: ${g.utilization_percent.toFixed(0)} % · Temperatura: ${g.temperature_c.toFixed(1)} °C`;
        } else {
          $("tel-gpu-name").textContent = "GPU no disponible";
          $("tel-vram-value").textContent = "sin dato";
          $("tel-vram-bar").style.width = "0%";
          $("tel-gpu-extra").textContent = cur.gpu_error || "GPU no disponible";
        }

        if (cur.cpu_percent != null) {
          $("tel-cpu-value").textContent = `${cur.cpu_percent.toFixed(1)} %`;
          const bar = $("tel-cpu-bar");
          bar.style.width = `${Math.min(100, cur.cpu_percent).toFixed(1)}%`;
          bar.className = `meter-fill ${levelClass(cur.cpu_percent)}`;
          $("tel-cpu-sub").textContent = `${hist.length}/${snap.capacity} muestras (ventana 5 min)`;
        } else {
          $("tel-cpu-value").textContent = "sin dato";
          $("tel-cpu-bar").style.width = "0%";
          $("tel-cpu-sub").textContent = cur.system_error || "Esperando segunda muestra para delta de CPU…";
        }

        if (cur.ram_used_kib != null && cur.ram_total_kib) {
          const ramPct = (cur.ram_used_kib / cur.ram_total_kib) * 100;
          $("tel-ram-value").textContent = `${(cur.ram_used_kib / 1048576).toFixed(2)} / ${(cur.ram_total_kib / 1048576).toFixed(2)} GiB`;
          const bar = $("tel-ram-bar");
          bar.style.width = `${Math.min(100, ramPct).toFixed(1)}%`;
          bar.className = `meter-fill ${levelClass(ramPct)}`;
          $("tel-ram-sub").textContent = `${ramPct.toFixed(1)} % en uso`;
        } else {
          $("tel-ram-value").textContent = "sin dato";
          $("tel-ram-bar").style.width = "0%";
        }

        renderLineSvg($("chart-vram-svg"), [{
          values: hist.map((s) => (s.gpu && s.gpu.memory_total_mib > 0) ? (s.gpu.memory_used_mib / s.gpu.memory_total_mib) * 100 : null),
          stroke: "#10b981",
          fill: "rgba(16, 185, 129, 0.14)",
        }], true);
        renderLineSvg($("chart-gpu-svg"), [{
          values: hist.map((s) => s.gpu ? s.gpu.utilization_percent : null),
          stroke: "#8b5cf6",
          fill: "rgba(139, 92, 246, 0.15)",
        }]);
        renderLineSvg($("chart-cpuram-svg"), [
          {
            values: hist.map((s) => s.cpu_percent),
            stroke: "#8b5cf6",
            fill: "rgba(139, 92, 246, 0.10)",
          },
          {
            values: hist.map((s) => (s.ram_used_kib != null && s.ram_total_kib) ? (s.ram_used_kib / s.ram_total_kib) * 100 : null),
            stroke: "#10b981",
            fill: null,
          },
        ]);
      }
