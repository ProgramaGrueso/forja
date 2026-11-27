      /* Vista Ajustes */
      let settingsFilledOnce = false;
      function fillSettingsForm(force) {
        if (!state.settings || (settingsFilledOnce && !force)) return;
        $("set-concurrency").value = state.settings.max_concurrency;
        $("set-store-dir").value = state.settings.store_dir;
        $("set-port").value = state.settings.port;
        $("set-quota-path").value = state.settings.quota_path;
        $("set-hide-mascot").checked = Boolean(state.settings.hide_mascot);
        $("set-hidden-profiles").value = (state.settings.hidden_profiles || []).join(", ");
        const allowedSensitive = Array.isArray(state.settings.sensitive_agents) && state.settings.sensitive_agents.length > 0
          ? state.settings.sensitive_agents
          : ["claude"];
        document.querySelectorAll('#set-sensitive-agents input[type="checkbox"]').forEach((cb) => {
          cb.checked = allowedSensitive.includes(cb.value);
        });
        renderModelRows();
        renderProfile();
        settingsFilledOnce = true;
      }

      function collectSensitiveAgents() {
        return Array.from(document.querySelectorAll('#set-sensitive-agents input[type="checkbox"]:checked'))
          .map((cb) => cb.value);
      }

      /* Modelo y esfuerzo por agente (Ajustes) */
      function renderModelRows() {
        const box = $("set-models-rows");
        const saved = (state.settings && state.settings.agent_models) || {};
        const savedEff = (state.settings && state.settings.agent_effort) || {};
        box.textContent = "";
        MODEL_AGENTS.forEach((name) => {
          const lab = document.createElement("label");
          lab.setAttribute("for", `set-model-${name}`);
          lab.className = "mono";
          lab.textContent = name;
          const inp = document.createElement("input");
          inp.id = `set-model-${name}`;
          inp.className = "input mono";
          inp.type = "text";
          inp.autocomplete = "off";
          inp.dataset.agent = name;
          inp.value = saved[name] || "";
          inp.placeholder = name === "hermes" ? "vacío = el del perfil" : (state.defaultModels[name] || "por defecto del agente");
          const levels = effortLevelsOf(name);
          let eff;
          if (levels.length) {
            eff = document.createElement("select");
            eff.id = `set-effort-${name}`;
            eff.className = "select";
            eff.dataset.effortAgent = name;
            eff.setAttribute("aria-label", `Esfuerzo de razonamiento de ${name}`);
            fillEffortSelect(eff, levels, "Esfuerzo: sin configurar", savedEff[name] || "");
          } else {
            eff = document.createElement("span");
            eff.className = "card-sub";
            eff.textContent = "esfuerzo: sin soporte";
          }
          const btn = document.createElement("button");
          btn.type = "button";
          btn.className = "btn";
          btn.textContent = "Restablecer";
          btn.setAttribute("aria-label", `Restablecer modelo y esfuerzo de ${name}`);
          btn.addEventListener("click", () => { inp.value = ""; if (eff.tagName === "SELECT") eff.value = ""; inp.focus(); });
          box.append(lab, inp, eff, btn);
        });
      }

      function collectAgentModels() {
        const out = {};
        document.querySelectorAll("#set-models-rows input[data-agent]").forEach((inp) => {
          const v = inp.value.trim();
          if (v) out[inp.dataset.agent] = v;
        });
        return out;
      }

      function collectAgentEffort() {
        const out = {};
        document.querySelectorAll("#set-models-rows select[data-effort-agent]").forEach((sel) => {
          if (sel.value) out[sel.dataset.effortAgent] = sel.value;
        });
        return out;
      }

      function updateDefaultModels() {
        const saved = (state.settings && state.settings.agent_models) || {};
        (state.agents || []).forEach((ag) => {
          if (ag.name !== "hermes" && MODEL_AGENTS.includes(ag.name) && !saved[ag.name] && ag.model) state.defaultModels[ag.name] = ag.model;
        });
        document.querySelectorAll("#set-models-rows input[data-agent]").forEach((inp) => {
          if (inp.dataset.agent === "hermes") return;
          inp.placeholder = state.defaultModels[inp.dataset.agent] || "por defecto del agente";
        });
      }

      /* Agente y perfil del orquestador */
      const profileInfo = { profile: null, source: null, allowed: [], profiles: [], agent: null, agent_source: null, agents_available: [], agents_supported: [] };
      const SOURCE_LABEL = { settings: "Ajustes", env: "variable FORJA_ORCHESTRATOR_PROFILE", default: "valor por defecto" };
      const AGENT_SOURCE_LABEL = { settings: "Ajustes", env: "variable FORJA_ORCHESTRATOR_AGENT", auto: "automático (primer agente instalado)" };
      async function loadProfile() {
        try {
          const r = await apiFetch("/api/orchestrator/profile");
          if (!r.ok) return;
          Object.assign(profileInfo, await r.json());
          renderProfile();
        } catch (_) {}
      }
      function fillSelect(sel, values, labelOf) {
        sel.textContent = "";
        values.forEach((v) => {
          const o = document.createElement("option");
          o.value = v; o.textContent = labelOf ? labelOf(v) : v;
          sel.appendChild(o);
        });
      }
      /* Agentes ofrecidos: los instalados (más el activo si ya no lo está). */
      function agentChoices() {
        const list = (profileInfo.agents_available || []).slice();
        if (profileInfo.agent && !list.includes(profileInfo.agent)) list.push(profileInfo.agent);
        return list;
      }
      function agentLabel(a) {
        return (profileInfo.agents_available || []).includes(a) ? a : `${a} (no instalado)`;
      }
      function toggleProfileFields() {
        const headIsHermes = $("orq-agent").value === "hermes";
        $("orq-profile").style.display = headIsHermes ? "" : "none";
        $("orq-profile-label").style.display = headIsHermes ? "" : "none";
        const sv = $("set-orch-agent").value;
        const setIsHermes = sv === "hermes" || (sv === "" && profileInfo.agent === "hermes");
        $("set-orch-profile-field").style.display = setIsHermes ? "" : "none";
      }
      function renderProfile() {
        const profiles = profileInfo.profiles && profileInfo.profiles.length ? profileInfo.profiles : (profileInfo.allowed || []);
        const profTxt = `perfil: ${profileInfo.profile || "?"} (${SOURCE_LABEL[profileInfo.source] || profileInfo.source || "?"})`;
        const srcTxt = `Activo: ${profileInfo.agent || "?"} · origen: ${AGENT_SOURCE_LABEL[profileInfo.agent_source] || profileInfo.agent_source || "?"}`
          + (profileInfo.agent === "hermes" ? ` · ${profTxt}` : "");

        const headAgent = $("orq-agent");
        fillSelect(headAgent, agentChoices(), agentLabel);
        if (profileInfo.agent) headAgent.value = profileInfo.agent;
        const head = $("orq-profile");
        fillSelect(head, profiles);
        if (profileInfo.profile) head.value = profileInfo.profile;
        $("orq-profile-source").textContent = srcTxt;
        orqHooks.refresh();

        const setAgent = $("set-orch-agent");
        const prevAgent = setAgent.value;
        setAgent.textContent = "";
        const autoA = document.createElement("option");
        autoA.value = ""; autoA.textContent = "Automático (variable de entorno o primer agente instalado)";
        setAgent.appendChild(autoA);
        (profileInfo.agents_supported || []).forEach((a) => {
          const o = document.createElement("option");
          o.value = a; o.textContent = agentLabel(a);
          setAgent.appendChild(o);
        });
        const configuredAgent = (state.settings && state.settings.orchestrator_agent) || "";
        setAgent.value = Array.from(setAgent.options).some((o) => o.value === configuredAgent) ? configuredAgent : (prevAgent || "");

        const set = $("set-orch-profile");
        const prevSel = set.value;
        set.textContent = "";
        const auto = document.createElement("option");
        auto.value = ""; auto.textContent = "Automático (variable de entorno o default)";
        set.appendChild(auto);
        profiles.forEach((p) => {
          const o = document.createElement("option");
          o.value = p; o.textContent = p;
          set.appendChild(o);
        });
        const configured = (state.settings && state.settings.orchestrator_profile) || "";
        set.value = Array.from(set.options).some((o) => o.value === configured) ? configured : (prevSel || "");
        $("set-orch-source").textContent = srcTxt;
        toggleProfileFields();
      }

      async function saveSettingsPatch(patch) {
        const curR = await apiFetch("/api/settings");
        if (!curR.ok) throw new Error(`No se pudieron leer los ajustes (HTTP ${curR.status})`);
        const cur = await curR.json();
        const payload = Object.assign({}, cur, patch);
        const resp = await apiFetch("/api/settings", {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(payload),
        });
        const data = await resp.json().catch(() => ({}));
        if (!resp.ok) throw new Error(data.error || `HTTP ${resp.status}`);
        state.settings = data.settings;
        return data;
      }

      $("settings-form").addEventListener("submit", async (e) => {
        e.preventDefault();
        const toast = $("settings-toast");
        const patch = {
          max_concurrency: Number($("set-concurrency").value),
          store_dir: $("set-store-dir").value.trim(),
          port: Number($("set-port").value),
          quota_path: $("set-quota-path").value.trim(),
          hide_mascot: $("set-hide-mascot").checked,
          agent_models: collectAgentModels(),
          agent_effort: collectAgentEffort(),
          orchestrator_profile: $("set-orch-profile").value || null,
          orchestrator_agent: $("set-orch-agent").value || null,
          hidden_profiles: $("set-hidden-profiles").value.split(",").map((x) => x.trim()).filter(Boolean),
          sensitive_agents: collectSensitiveAgents(),
        };
        const profileChanged = (state.settings && (state.settings.orchestrator_profile || "")) !== ($("set-orch-profile").value || "");
        const agentChanged = (state.settings && (state.settings.orchestrator_agent || "")) !== ($("set-orch-agent").value || "");
        if ((profileChanged || agentChanged) && !window.confirm("Cambiar el agente o el perfil del orquestador reinicia las sesiones de sus conversaciones. ¿Continuar?")) return;
        let data;
        try {
          data = await saveSettingsPatch(patch);
        } catch (err) {
          toast.className = "toast-banner show err";
          toast.textContent = err.message || "Ajustes inválidos";
          return;
        }
        renderMascot();
        await refreshQuota(true);
        await refreshCore();
        fillSettingsForm(true);
        await loadProfile();
        orqApi.reload();
        const restart = (data.restart_required || []);
        toast.className = "toast-banner show ok";
        toast.textContent = restart.length > 0
          ? `Ajustes guardados. Reinicia forja serve para aplicar: ${restart.join(", ")}.`
          : "Ajustes aplicados y guardados.";
      });

      $("set-hide-mascot").addEventListener("change", () => {
        if (state.settings) {
          state.settings.hide_mascot = $("set-hide-mascot").checked;
          renderMascot();
        }
      });
