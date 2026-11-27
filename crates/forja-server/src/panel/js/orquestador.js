      /* Orquestador: conversaciones + chat + llamada de voz (Web Speech API del navegador) */
      (function orquestador() {
        const SR = window.SpeechRecognition || window.webkitSpeechRecognition;
        const log = $("orq-log");
        const input = $("orq-input");
        const statusEl = $("orq-status");
        const callBtn = $("orq-call");
        const listEl = $("orq-conv-list");
        const suggest = $("orq-suggest");
        const LS_KEY = "forja_orq_conv";
        const INTRO = "Háblale o escríbele. Él delega las tareas en los agentes y revisa el resultado antes de responderte. Escribe / para ver los comandos.";
        const COMMANDS = [
          ["/new", "Crea una conversación nueva"],
          ["/help", "Lista los comandos"],
          ["/rename ", "Renombra: /rename <título>"],
          ["/agents", "Estado de los agentes"],
          ["/stop", "Cancela las ejecuciones en curso"],
          ["/model ", "Modelo del orquestador: /model <modelo> (reset = Ajustes)"],
          ["/effort ", "Esfuerzo de razonamiento: /effort <nivel> (reset = Ajustes)"],
          ["/agent", "Disponible pronto"],
        ];
        const modelSel = $("orq-model");
        const effortSel = $("orq-effort");
        const OTHER = "__otro__";
        const choice = new Map();   // id -> { modelo?, esfuerzo? } elegido y aún no enviado
        let convs = [];
        let activeId = null;
        const busy = new Set();      // conversaciones con una petición mía en vuelo
        const pending = new Map();   // id -> último mensaje de usuario enviado
        let serverBusyPrev = false;
        let inCall = false;
        let rec = null;
        let voiceStatus = "";
        let loadSeq = 0;

        const isBusy = (id) => busy.has(id) || Boolean((convs.find((c) => c.id === id) || {}).ocupada);
        const lsGet = () => { try { return localStorage.getItem(LS_KEY); } catch (_) { return null; } };
        const lsSet = (v) => { try { if (v) localStorage.setItem(LS_KEY, v); else localStorage.removeItem(LS_KEY); } catch (_) {} };

        function add(kind, text) {
          const d = document.createElement("div");
          d.className = `orq-msg ${kind}`;
          d.textContent = text;
          log.appendChild(d);
          log.scrollTop = log.scrollHeight;
        }
        function lastMsgText() {
          const last = log.lastElementChild;
          return last ? last.textContent : null;
        }
        function updateStatus() {
          const thinking = activeId && isBusy(activeId);
          statusEl.textContent = thinking ? "El orquestador está pensando..." : voiceStatus;
          $("orq-send").classList.toggle("orq-send-busy", Boolean(thinking));
          $("orq-send").setAttribute("aria-disabled", thinking ? "true" : "false");
        }
        function setStatus(t) { voiceStatus = t || ""; updateStatus(); }

        /* Composer: [Modelo ▾] [Esfuerzo ▾] por conversación */
        const MODEL_NICE = { sonnet: "Sonnet", opus: "Opus", haiku: "Haiku" };
        const modelLabel = (m) => MODEL_NICE[m] || m;
        function modelCandidates() {
          const out = (profileInfo.model_options || []).slice();
          const agent = profileInfo.agent;
          const prov = { claude: (p) => p === "anthropic", agy: (p) => p === "gemini", local: (p) => p === "local", opencode: (p) => p.startsWith("opencode") }[agent];
          if (prov && state.models && state.models.providers) {
            state.models.providers.filter((p) => prov(p.provider) && p.status === "ok").forEach((p) => {
              (p.models || []).slice(0, 60).forEach((m) => { if (!out.includes(m.id)) out.push(m.id); });
            });
          }
          if (profileInfo.model && !out.includes(profileInfo.model)) out.push(profileInfo.model);
          return out;
        }
        function renderComposer() {
          const act = convs.find((c) => c.id === activeId) || {};
          const ch = (activeId && choice.get(activeId)) || {};
          const curModel = "modelo" in ch ? ch.modelo : (act.modelo || "");
          const curEffort = "esfuerzo" in ch ? ch.esfuerzo : (act.esfuerzo || "");
          const editable = profileInfo.model_editable !== false && Boolean(profileInfo.agent);
          modelSel.hidden = !editable;
          if (editable) {
            modelSel.textContent = "";
            const b = document.createElement("option");
            b.value = ""; b.textContent = `${modelLabel(profileInfo.model || "Modelo")} (Ajustes)`;
            modelSel.appendChild(b);
            const cands = modelCandidates().filter((m) => m !== profileInfo.model);
            if (curModel && !cands.includes(curModel) && curModel !== profileInfo.model) cands.unshift(curModel);
            cands.forEach((m) => { const o = document.createElement("option"); o.value = m; o.textContent = modelLabel(m); modelSel.appendChild(o); });
            const other = document.createElement("option");
            other.value = OTHER; other.textContent = "otro…";
            modelSel.appendChild(other);
            modelSel.value = curModel === profileInfo.model ? "" : curModel;
          }
          const levels = profileInfo.effort_levels || [];
          effortSel.hidden = levels.length === 0;
          if (levels.length) {
            fillEffortSelect(effortSel, levels, `Esfuerzo${profileInfo.effort ? " (" + (EFFORT_SHORT[profileInfo.effort] || profileInfo.effort) + ")" : ""}`, curEffort);
            if (profileInfo.agent === "opencode") {
              const other = document.createElement("option");
              other.value = OTHER; other.textContent = "otro…";
              effortSel.appendChild(other);
              effortSel.value = curEffort;
            }
          }
        }
        orqHooks.refresh = renderComposer;
        function setChoice(key, value) {
          if (!activeId) return;
          const ch = choice.get(activeId) || {};
          ch[key] = value;
          choice.set(activeId, ch);
          setStatus(value === "" ? "Se volverá a Ajustes en el próximo mensaje." : "Se aplicará en el próximo mensaje (sin reiniciar la conversación).");
          renderComposer();
        }
        modelSel.addEventListener("change", () => {
          let v = modelSel.value;
          if (v === OTHER) {
            v = (window.prompt("Modelo para este orquestador (texto libre):", "") || "").trim();
            if (!v) { renderComposer(); return; }
          }
          setChoice("modelo", v);
        });
        effortSel.addEventListener("change", () => {
          let v = effortSel.value;
          if (v === OTHER) {
            v = (window.prompt("Variante de razonamiento (a-z, 0-9, - o _; máx. 20):", "") || "").trim().toLowerCase();
            if (!v) { renderComposer(); return; }
          }
          setChoice("esfuerzo", v);
        });

        /* Lista lateral */
        function renderList() {
          $("orq-conv-count").textContent = String(convs.length);
          listEl.textContent = "";
          if (convs.length === 0) {
            const e = document.createElement("div");
            e.className = "muted";
            e.style.cssText = "font-size:11.5px;padding:6px;";
            e.textContent = "Sin conversaciones. Crea una con + Nueva.";
            listEl.appendChild(e);
          }
          convs.forEach((c) => {
            const row = document.createElement("div");
            row.className = "orq-conv" + (c.id === activeId ? " active" : "");
            row.setAttribute("role", "listitem");
            row.tabIndex = 0;
            row.dataset.convId = c.id;
            const dot = document.createElement("span");
            dot.className = "dot " + (isBusy(c.id) ? "busy" : "idle");
            dot.title = isBusy(c.id) ? "Pensando" : "Libre";
            const main = document.createElement("div");
            main.className = "orq-conv-main";
            const t = document.createElement("div");
            t.className = "orq-conv-title";
            t.textContent = c.titulo || "(sin título)";
            const sub = document.createElement("div");
            sub.className = "orq-conv-sub";
            sub.textContent = `${c.mensajes} msj · ${c.id.slice(0, 8)}`;
            main.append(t, sub);
            const del = document.createElement("button");
            del.type = "button";
            del.className = "icon-btn orq-conv-del";
            del.textContent = "✕";
            del.setAttribute("aria-label", `Borrar conversación ${c.titulo || c.id}`);
            del.addEventListener("click", (ev) => { ev.stopPropagation(); removeConv(c.id); });
            row.append(dot, main, del);
            row.addEventListener("click", () => select(c.id));
            row.addEventListener("keydown", (ev) => {
              if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); select(c.id); }
            });
            listEl.appendChild(row);
          });
          renderComposer();
          const act = convs.find((c) => c.id === activeId);
          $("orq-conv-title").textContent = act ? (act.titulo || "(sin título)") : "Sin conversación";
          $("orq-rename").disabled = !act;
        }

        async function loadList() {
          try {
            const r = await apiFetch("/api/orchestrator/conversations");
            if (!r.ok) return false;
            const list = await r.json();
            list.sort((a, b) => (b.actualizado || 0) - (a.actualizado || 0));
            convs = list;
            renderList();
            updateStatus();
            return true;
          } catch (_) { return false; }
        }

        function renderHistory(detail) {
          log.textContent = "";
          const hist = (detail && detail.historial) || [];
          if (hist.length === 0) add("sys", INTRO);
          hist.forEach((m) => add(m.rol === "usuario" ? "user" : m.rol === "orquestador" ? "bot" : "sys", m.texto));
          const p = pending.get(detail && detail.resumen && detail.resumen.id);
          if (p && !(hist.length && hist[hist.length - 1].rol === "usuario" && hist[hist.length - 1].texto === p)) add("user", p);
        }

        async function loadDetail(id) {
          const seq = ++loadSeq;
          try {
            const r = await apiFetch(`/api/orchestrator/conversations/${encodeURIComponent(id)}`);
            if (seq !== loadSeq || id !== activeId) return;
            if (r.status === 404) { await handleGone(id); return; }
            if (!r.ok) { add("sys", `No se pudo cargar la conversación (HTTP ${r.status}).`); return; }
            const d = await r.json();
            if (seq !== loadSeq || id !== activeId) return;
            renderHistory(d);
          } catch (e) {
            if (id === activeId) add("sys", `Error de red: ${e.message}`);
          }
        }

        async function handleGone(id) {
          add("sys", "Esa conversación ya no existe (¿se borró en otra pestaña?).");
          await loadList();
          if (activeId === id) {
            activeId = null;
            if (convs.length) await select(convs[0].id); else { lsSet(null); renderList(); }
          }
        }

        async function select(id) {
          activeId = id;
          lsSet(id);
          renderList();
          updateStatus();
          log.textContent = "";
          await loadDetail(id);
        }

        async function createConv() {
          try {
            const r = await apiFetch("/api/orchestrator/conversations", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: "{}",
            });
            const data = await r.json().catch(() => ({}));
            if (r.status === 409) { add("sys", data.error || "Límite de 20 conversaciones alcanzado: borra alguna para crear otra."); return null; }
            if (!r.ok) { add("sys", `No se pudo crear la conversación: ${data.error || "HTTP " + r.status}`); return null; }
            await loadList();
            await select(data.id);
            return data.id;
          } catch (e) { add("sys", `Error de red: ${e.message}`); return null; }
        }

        async function removeConv(id) {
          const c = convs.find((x) => x.id === id);
          if (!window.confirm(`¿Borrar la conversación «${(c && c.titulo) || id}»? No se puede deshacer.`)) return;
          try {
            const r = await apiFetch(`/api/orchestrator/conversations/${encodeURIComponent(id)}`, { method: "DELETE" });
            if (!r.ok && r.status !== 404) {
              const data = await r.json().catch(() => ({}));
              add("sys", `No se pudo borrar: ${data.error || "HTTP " + r.status}`);
              return;
            }
          } catch (e) { add("sys", `Error de red: ${e.message}`); return; }
          busy.delete(id); pending.delete(id); choice.delete(id);
          await loadList();
          if (activeId === id) {
            activeId = null;
            if (convs.length) await select(convs[0].id);
            else { lsSet(null); log.textContent = ""; add("sys", INTRO); renderList(); }
          }
        }

        function speak(text) {
          return new Promise((resolve) => {
            if (!window.speechSynthesis) { resolve(); return; }
            window.speechSynthesis.cancel();
            const u = new SpeechSynthesisUtterance(text);
            u.lang = "es-419";
            const voices = window.speechSynthesis.getVoices().filter((v) => v.lang.toLowerCase().startsWith("es"));
            if (voices.length) u.voice = voices[0];
            u.onend = resolve;
            u.onerror = resolve;
            window.speechSynthesis.speak(u);
          });
        }

        async function send(text) {
          text = (text || "").trim();
          if (!text) return;
          if (!activeId) {
            const nid = await createConv();
            if (!nid) return;
          }
          const id = activeId;
          if (isBusy(id)) { add("sys", "Esta conversación está pensando; espera su respuesta o cambia a otra."); return; }
          const isCmd = text.startsWith("/");
          busy.add(id);
          pending.set(id, text);
          add("user", text);
          renderList();
          updateStatus();
          let spoken = null;
          try {
            const r = await apiFetch("/api/orchestrator/chat", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify(Object.assign({ message: text, conversation_id: id }, isCmd ? {} : (choice.get(id) || {}))),
            });
            const data = await r.json().catch(() => ({}));
            if (r.status === 404) {
              busy.delete(id); pending.delete(id);
              if (id === activeId) await handleGone(id);
              return;
            }
            if (r.status === 409) {
              if (id === activeId) add("sys", data.error || "La conversación está ocupada o se alcanzó el límite de 20 conversaciones.");
              return;
            }
            if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
            const reply = data.reply || "";
            pending.delete(id);
            if (!isCmd) choice.delete(id);
            if (data.new_conversation && data.conversation_id) {
              await loadList();
              await select(data.conversation_id);
              if (lastMsgText() !== reply) add("sys", reply);
            } else if (id === activeId) {
              await loadList();
              await loadDetail(id);
              if (lastMsgText() !== reply) add(isCmd ? "sys" : "bot", reply);
              if (!isCmd) spoken = reply;
            } else {
              await loadList();
            }
          } catch (e) {
            if (id === activeId) add("sys", `Error: ${e.message}`);
          } finally {
            busy.delete(id);
            pending.delete(id);
            renderList();
            updateStatus();
          }
          if (inCall && id === activeId && spoken) {
            setStatus("Hablando...");
            await speak(spoken);
            setStatus("");
          }
          if (inCall) listen();
        }

        /* Autocompletado de comandos */
        function updateSuggest() {
          const v = input.value;
          suggest.textContent = "";
          if (!v.startsWith("/") || v.includes(" ")) { suggest.classList.remove("open"); return; }
          const matches = COMMANDS.filter(([c]) => c.startsWith(v.toLowerCase()));
          if (matches.length === 0) { suggest.classList.remove("open"); return; }
          matches.forEach(([c, d]) => {
            const b = document.createElement("button");
            b.type = "button";
            b.setAttribute("role", "option");
            const n = document.createElement("span");
            n.className = "mono";
            n.textContent = c.trim();
            const t = document.createElement("span");
            t.className = "muted";
            t.textContent = d;
            b.append(n, t);
            b.addEventListener("click", () => { input.value = c; autosize(); suggest.classList.remove("open"); input.focus(); });
            suggest.appendChild(b);
          });
          suggest.classList.add("open");
        }
        /* La caja crece con el texto hasta el max-height del CSS. */
        function autosize() {
          input.style.height = "";
          if (!input.value) return;
          const border = input.offsetHeight - input.clientHeight;
          input.style.height = `${input.scrollHeight + border}px`;
        }
        input.addEventListener("input", () => { autosize(); updateSuggest(); });
        input.addEventListener("keydown", (e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            // Enter envía; Shift+Enter baja una línea. Mientras un método de entrada
            // compone un carácter, Enter solo lo confirma.
            if (e.isComposing || e.keyCode === 229) return;
            e.preventDefault();
            $("orq-form").requestSubmit();
          } else if (e.key === "Tab" && !e.shiftKey && suggest.classList.contains("open")) {
            e.preventDefault();
            const first = suggest.querySelector("button");
            if (first) first.click();
          } else if (e.key === "Escape") {
            suggest.classList.remove("open");
          }
        });

        /* Voz */
        function listen() {
          if (!inCall || !SR || (activeId && isBusy(activeId))) return;
          rec = new SR();
          rec.lang = "es-419";
          rec.interimResults = true;
          rec.continuous = false;
          let finalText = "";
          rec.onresult = (ev) => {
            let interim = "";
            for (let i = ev.resultIndex; i < ev.results.length; i++) {
              const t = ev.results[i][0].transcript;
              if (ev.results[i].isFinal) finalText += t; else interim += t;
            }
            setStatus(`Escuchando: ${finalText || interim}`);
          };
          rec.onerror = (ev) => {
            if (ev.error === "not-allowed" || ev.error === "service-not-allowed") {
              add("sys", "El navegador no dio permiso al micrófono.");
              endCall();
            }
          };
          rec.onend = () => {
            if (!inCall) return;
            if (finalText.trim()) send(finalText);
            else if (!(activeId && isBusy(activeId))) listen();
          };
          try { rec.start(); setStatus("Escuchando..."); } catch (e) { /* ya activo */ }
        }

        function endCall() {
          inCall = false;
          callBtn.classList.remove("on");
          callBtn.setAttribute("aria-label", "Iniciar llamada de voz");
          callBtn.setAttribute("aria-pressed", "false");
          if (rec) { try { rec.abort(); } catch (e) {} }
          if (window.speechSynthesis) window.speechSynthesis.cancel();
          setStatus("");
        }

        callBtn.addEventListener("click", () => {
          if (inCall) { endCall(); add("sys", "Llamada terminada."); return; }
          if (!SR) {
            add("sys", "Este navegador no soporta reconocimiento de voz. Usa Chrome, Edge o Zen/Chromium con internet.");
            return;
          }
          inCall = true;
          callBtn.classList.add("on");
          callBtn.setAttribute("aria-label", "Terminar llamada de voz");
          callBtn.setAttribute("aria-pressed", "true");
          add("sys", "Llamada iniciada. Habla cuando quieras (opera sobre la conversación activa).");
          listen();
        });
        $("orq-form").addEventListener("submit", (e) => {
          e.preventDefault();
          const t = input.value;
          input.value = "";
          autosize();
          suggest.classList.remove("open");
          send(t);
        });
        $("orq-reset").addEventListener("click", createConv);
        $("orq-new").addEventListener("click", createConv);
        $("orq-rename").addEventListener("click", () => {
          const act = convs.find((c) => c.id === activeId);
          if (!act) return;
          const t = window.prompt("Nuevo título de la conversación:", act.titulo || "");
          if (t && t.trim()) send(`/rename ${t.trim()}`);
        });

        /* Selector de agente del orquestador (cabecera) */
        $("orq-agent").addEventListener("change", async () => {
          const sel = $("orq-agent");
          const msg = $("orq-profile-msg");
          const wanted = sel.value;
          if (!window.confirm(`Cambiar el agente del orquestador a «${wanted}» reinicia las sesiones de las conversaciones. ¿Continuar?`)) {
            if (profileInfo.agent) sel.value = profileInfo.agent;
            toggleProfileFields();
            return;
          }
          try {
            await saveSettingsPatch({ orchestrator_agent: wanted });
            msg.textContent = `Agente cambiado a ${wanted}.`;
            await loadProfile();
            fillSettingsForm(true);
            await loadList();
          } catch (e) {
            msg.textContent = `Error: ${e.message}`;
            if (profileInfo.agent) sel.value = profileInfo.agent;
            toggleProfileFields();
          }
        });
        $("set-orch-agent").addEventListener("change", toggleProfileFields);

        /* Selector de perfil de Hermes del orquestador (cabecera) */
        $("orq-profile").addEventListener("change", async () => {
          const sel = $("orq-profile");
          const msg = $("orq-profile-msg");
          const wanted = sel.value;
          if (!window.confirm(`Cambiar el orquestador a «${wanted}» reinicia las sesiones de las conversaciones. ¿Continuar?`)) {
            if (profileInfo.profile) sel.value = profileInfo.profile;
            return;
          }
          try {
            await saveSettingsPatch({ orchestrator_profile: wanted });
            msg.textContent = `Perfil cambiado a ${wanted}.`;
            await loadProfile();
            fillSettingsForm(true);
            await loadList();
          } catch (e) {
            msg.textContent = `Error: ${e.message}`;
            if (profileInfo.profile) sel.value = profileInfo.profile;
          }
        });

        async function init() {
          await loadList();
          if (convs.length === 0) { renderList(); log.textContent = ""; add("sys", INTRO); return; }
          const saved = lsGet();
          const pick = convs.find((c) => c.id === saved) || convs[0];
          await select(pick.id);
        }
        orqApi.reload = async () => {
          await loadList();
          if (activeId && !convs.some((c) => c.id === activeId)) { activeId = null; if (convs.length) await select(convs[0].id); }
          else if (activeId && !busy.has(activeId)) await loadDetail(activeId);
        };
        // Sondeo ligero mientras la vista está abierta: refleja conversaciones ocupadas desde otra pestaña.
        setInterval(async () => {
          if (state.view !== "orquestador") return;
          const wasBusy = activeId ? isBusy(activeId) : false;
          await loadList();
          const nowBusy = activeId ? isBusy(activeId) : false;
          if (activeId && !busy.has(activeId) && wasBusy && !nowBusy) loadDetail(activeId);
          serverBusyPrev = nowBusy;
        }, 4000);
        init();
        loadProfile();
      })();
