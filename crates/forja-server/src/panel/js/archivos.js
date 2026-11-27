      /* Modo IDE: selector de carpeta (workspace) y vista Archivos (árbol, pestañas y editor) */
      (function archivosIde() {
        const lsGet = (k) => { try { return localStorage.getItem(k); } catch (_) { return null; } };
        const lsSet = (k, v) => { try { localStorage.setItem(k, v); } catch (_) {} };

        const wsBar = $("workspace-bar");
        const wsNameEl = $("ws-name");
        const wsBranchEl = $("ws-branch");
        const btnOpenWs = $("btn-open-workspace");
        const wsModal = $("workspace-modal");
        const wsCloseBtn = $("ws-modal-close");
        const wsPathInput = $("ws-path-input");
        const wsOpenPathBtn = $("ws-open-path-btn");
        const wsRecentList = $("ws-recent-list");
        const wsBrowseCurrent = $("ws-browse-current");
        const wsBrowseUp = $("ws-browse-up");
        const wsSelectCurrent = $("ws-select-current");
        const wsBrowseList = $("ws-browse-list");
        const wsError = $("ws-error");

        const sourceSel = $("files-source");
        const searchInput = $("files-search");
        const refreshBtn = $("files-refresh");
        const treeEl = $("files-tree");
        const tabsEl = $("files-tabs");
        const activePathEl = $("files-active-path");
        const langEl = $("files-lang");
        const roBadgeEl = $("files-ro-badge");
        const citeBtn = $("files-cite-btn");
        const saveBtn = $("files-save-btn");
        const saveStatusEl = $("files-save-status");
        const emptyEl = $("files-empty");
        const editorBoxEl = $("files-editor-box");
        const gutterEl = $("files-gutter");
        const editorEl = $("files-editor");

        let activeWorkspace = null;
        let recentWorkspaces = [];
        let browseState = { current: "", parent: null, is_git: false, entries: [] };

        let currentRunId = "";
        let treeCache = new Map();
        let expandedDirs = new Set();
        let searchResults = null;
        let searchTimer = null;

        let tabs = [];
        let activeTabKey = null;
        let saving = false;
        let lastLineCount = 0;
        let treeLoaded = false;

        function tabKey(runId, path) {
          return `${runId || ""}::${path}`;
        }

        function baseName(path) {
          const parts = String(path || "").split("/").filter(Boolean);
          return parts.length ? parts[parts.length - 1] : path || "";
        }

        function fmtSize(bytes) {
          if (bytes == null || !Number.isFinite(bytes)) return "";
          if (bytes < 1024) return `${bytes} B`;
          if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
          return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
        }

        function tabsStorageKey(wsPath) {
          return `forja_ide_tabs:${wsPath || "default"}`;
        }

        function persistTabs() {
          if (!activeWorkspace || !activeWorkspace.path) return;
          const baseTabs = tabs.filter((t) => !t.runId).map((t) => t.path);
          lsSet(tabsStorageKey(activeWorkspace.path), JSON.stringify({
            paths: baseTabs,
            active: (activeTab() && !activeTab().runId) ? activeTab().path : (baseTabs[0] || null),
          }));
        }

        function activeTab() {
          if (!activeTabKey) return null;
          return tabs.find((t) => t.key === activeTabKey) || null;
        }

        function setSaveStatus(msg, kind) {
          saveStatusEl.textContent = msg || "";
          saveStatusEl.className = "ide-save-status" + (kind ? ` ${kind}` : "");
        }

        function showWsError(msg) {
          if (!msg) {
            wsError.textContent = "";
            wsError.classList.remove("show");
            return;
          }
          wsError.textContent = msg;
          wsError.classList.add("show");
        }

        /* --- Workspace --- */
        function renderWorkspaceBar() {
          if (!activeWorkspace) {
            wsNameEl.textContent = "Sin carpeta";
            wsBar.title = "Carpeta activa del proyecto";
            wsBranchEl.textContent = "sin git";
            wsBranchEl.classList.remove("git");
            return;
          }
          wsNameEl.textContent = activeWorkspace.name || baseName(activeWorkspace.path) || activeWorkspace.path;
          wsBar.title = activeWorkspace.path || "";
          if (activeWorkspace.is_git) {
            wsBranchEl.textContent = activeWorkspace.branch || "git";
            wsBranchEl.classList.add("git");
          } else {
            wsBranchEl.textContent = "sin git";
            wsBranchEl.classList.remove("git");
          }
        }

        async function loadWorkspaces(restoreSavedTabs) {
          try {
            const r = await apiFetch("/api/workspaces");
            if (!r.ok) return;
            const data = await r.json();
            activeWorkspace = data.active || null;
            recentWorkspaces = Array.isArray(data.recent) ? data.recent : [];
            renderWorkspaceBar();
            if (wsModal.classList.contains("open")) renderRecentWorkspaces();
            if (restoreSavedTabs && activeWorkspace && activeWorkspace.path) {
              await restoreWorkspaceTabs(activeWorkspace.path);
            }
          } catch (_) {}
        }

        async function restoreWorkspaceTabs(wsPath) {
          let saved = null;
          try { saved = JSON.parse(lsGet(tabsStorageKey(wsPath)) || "null"); } catch (_) {}
          if (!saved || !Array.isArray(saved.paths) || saved.paths.length === 0) return;
          for (const p of saved.paths.slice(0, 12)) {
            if (typeof p === "string" && p) {
              await openFile(p, "", true);
            }
          }
          if (saved.active) {
            const k = tabKey("", saved.active);
            if (tabs.some((t) => t.key === k)) {
              activeTabKey = k;
              renderTabs();
              renderEditor();
            }
          }
        }

        function renderRecentWorkspaces() {
          wsRecentList.textContent = "";
          if (!recentWorkspaces.length) {
            const d = document.createElement("div");
            d.className = "ide-tree-empty";
            d.textContent = "Sin carpetas recientes todavía.";
            wsRecentList.appendChild(d);
            return;
          }
          recentWorkspaces.forEach((ws) => {
            const btn = document.createElement("button");
            btn.type = "button";
            btn.className = "ws-item";
            const main = document.createElement("div");
            main.className = "ws-item-main";
            const name = document.createElement("div");
            name.className = "ws-item-name";
            name.textContent = ws.name || baseName(ws.path) || ws.path;
            const path = document.createElement("div");
            path.className = "ws-item-path";
            path.textContent = ws.path;
            main.append(name, path);
            btn.appendChild(main);
            if (ws.is_git) {
              const pill = document.createElement("span");
              pill.className = "ws-git-pill";
              pill.textContent = ws.branch || "git";
              btn.appendChild(pill);
            }
            btn.addEventListener("click", () => selectWorkspace(ws.path));
            wsRecentList.appendChild(btn);
          });
        }

        async function browseDir(path) {
          showWsError("");
          const qs = path ? `?path=${encodeURIComponent(path)}` : "";
          try {
            const r = await apiFetch(`/api/workspaces/browse${qs}`);
            if (!r.ok) {
              const d = await r.json().catch(() => ({ error: `HTTP ${r.status}` }));
              showWsError(d.error || `No se pudo listar la carpeta (HTTP ${r.status})`);
              return;
            }
            browseState = await r.json();
            wsBrowseCurrent.textContent = browseState.current || "—";
            wsBrowseCurrent.title = browseState.current || "";
            wsBrowseUp.disabled = !browseState.parent;
            wsBrowseList.textContent = "";
            const entries = Array.isArray(browseState.entries) ? browseState.entries : [];
            if (!entries.length) {
              const d = document.createElement("div");
              d.className = "ide-tree-empty";
              d.textContent = "No hay subcarpetas.";
              wsBrowseList.appendChild(d);
              return;
            }
            entries.forEach((entry) => {
              const btn = document.createElement("button");
              btn.type = "button";
              btn.className = "ws-item";
              const main = document.createElement("div");
              main.className = "ws-item-main";
              const name = document.createElement("div");
              name.className = "ws-item-name";
              name.textContent = entry.name;
              main.appendChild(name);
              btn.appendChild(main);
              if (entry.is_git) {
                const pill = document.createElement("span");
                pill.className = "ws-git-pill";
                pill.textContent = "git";
                btn.appendChild(pill);
              }
              btn.addEventListener("click", () => browseDir(entry.path));
              wsBrowseList.appendChild(btn);
            });
          } catch (_) {
            showWsError("Error de conexión al explorar carpetas.");
          }
        }

        async function selectWorkspace(path) {
          const target = String(path || "").trim();
          if (!target) {
            showWsError("Introduce una ruta válida.");
            return;
          }
          showWsError("");
          try {
            const r = await apiFetch("/api/workspaces/select", {
              method: "POST",
              headers: { "content-type": "application/json" },
              body: JSON.stringify({ path: target }),
            });
            if (!r.ok) {
              const d = await r.json().catch(() => ({ error: `HTTP ${r.status}` }));
              if (r.status === 409) {
                showWsError(d.error || "No se puede cambiar de carpeta mientras hay ejecuciones activas.");
              } else {
                showWsError(d.error || `Error al cambiar de carpeta (HTTP ${r.status})`);
              }
              return;
            }
            const data = await r.json();
            activeWorkspace = data.active || null;
            recentWorkspaces = Array.isArray(data.recent) ? data.recent : [];
            renderWorkspaceBar();
            closeWorkspaceModal();

            currentRunId = "";
            sourceSel.value = "";
            searchInput.value = "";
            searchResults = null;
            treeCache.clear();
            expandedDirs.clear();
            tabs = [];
            activeTabKey = null;
            setSaveStatus("", "");
            renderTabs();
            renderEditor();
            await loadTreeDir("");
            if (activeWorkspace && activeWorkspace.path) {
              await restoreWorkspaceTabs(activeWorkspace.path);
            }
            if (filesApi.onWorkspaceChanged) filesApi.onWorkspaceChanged(activeWorkspace);
            refreshCore();
          } catch (_) {
            showWsError("Error de red al cambiar de carpeta.");
          }
        }

        function openWorkspaceModal() {
          showWsError("");
          wsPathInput.value = (activeWorkspace && activeWorkspace.path) || "";
          wsModal.classList.add("open");
          loadWorkspaces(false).then(() => {
            renderRecentWorkspaces();
            browseDir((activeWorkspace && activeWorkspace.path) || "");
          });
        }

        function closeWorkspaceModal() {
          wsModal.classList.remove("open");
        }

        /* --- Árbol y búsqueda --- */
        async function loadTreeDir(dirPath) {
          const params = new URLSearchParams();
          if (dirPath) params.set("path", dirPath);
          if (currentRunId) params.set("run_id", currentRunId);
          const qs = params.toString() ? `?${params.toString()}` : "";
          try {
            const r = await apiFetch(`/api/files/tree${qs}`);
            if (!r.ok) {
              if (!dirPath) {
                treeEl.innerHTML = `<div class="ide-tree-empty">No se pudo cargar el árbol (HTTP ${r.status}).</div>`;
              }
              return;
            }
            const data = await r.json();
            treeCache.set(dirPath, Array.isArray(data.entries) ? data.entries : []);
            treeLoaded = true;
            renderTree();
          } catch (_) {
            if (!dirPath) {
              treeEl.innerHTML = `<div class="ide-tree-empty">Error de red al cargar archivos.</div>`;
            }
          }
        }

        async function runSearch(query) {
          const q = query.trim();
          if (!q) {
            searchResults = null;
            renderTree();
            return;
          }
          const params = new URLSearchParams({ q });
          if (currentRunId) params.set("run_id", currentRunId);
          try {
            const r = await apiFetch(`/api/files/search?${params.toString()}`);
            if (!r.ok) return;
            const data = await r.json();
            if (searchInput.value.trim() !== q) return;
            searchResults = Array.isArray(data.matches) ? data.matches : [];
            renderTree();
          } catch (_) {}
        }

        function iconSvg(isDir, isOpen) {
          if (isDir) {
            return isOpen
              ? `<svg class="ide-node-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v1H3V7z"/><path d="M3 10h18l-1.5 8a2 2 0 0 1-2 1.7H6.5a2 2 0 0 1-2-1.7L3 10z"/></svg>`
              : `<svg class="ide-node-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z"/></svg>`;
          }
          return `<svg class="ide-node-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/></svg>`;
        }

        function renderTreeNodes(dirPath, depth, frag) {
          const entries = treeCache.get(dirPath);
          if (!entries) return;
          entries.forEach((entry) => {
            const isOpen = entry.is_dir && expandedDirs.has(entry.path);
            const btn = document.createElement("button");
            btn.type = "button";
            const key = tabKey(currentRunId, entry.path);
            btn.className = "ide-node" + (entry.is_dir ? " is-dir" : "") + (activeTabKey === key ? " active" : "");
            btn.style.paddingLeft = `${8 + depth * 14}px`;
            btn.title = entry.path;
            btn.innerHTML = `
              <span class="ide-node-caret">${entry.is_dir ? (isOpen ? "▾" : "▸") : ""}</span>
              ${iconSvg(entry.is_dir, isOpen)}
              <span class="ide-node-name">${esc(entry.name)}</span>
              ${!entry.is_dir && entry.size != null ? `<span class="ide-node-size">${esc(fmtSize(entry.size))}</span>` : ""}
            `;
            btn.addEventListener("click", async () => {
              if (entry.is_dir) {
                if (expandedDirs.has(entry.path)) {
                  expandedDirs.delete(entry.path);
                  renderTree();
                } else {
                  expandedDirs.add(entry.path);
                  if (!treeCache.has(entry.path)) {
                    await loadTreeDir(entry.path);
                  } else {
                    renderTree();
                  }
                }
              } else {
                await openFile(entry.path, currentRunId, false);
              }
            });
            frag.appendChild(btn);
            if (isOpen) {
              renderTreeNodes(entry.path, depth + 1, frag);
            }
          });
        }

        function renderTree() {
          treeEl.textContent = "";
          if (searchResults !== null) {
            if (!searchResults.length) {
              const d = document.createElement("div");
              d.className = "ide-tree-empty";
              d.textContent = "Sin coincidencias.";
              treeEl.appendChild(d);
              return;
            }
            const frag = document.createDocumentFragment();
            searchResults.forEach((m) => {
              const btn = document.createElement("button");
              btn.type = "button";
              const key = tabKey(currentRunId, m.path);
              btn.className = "ide-node" + (m.is_dir ? " is-dir" : "") + (activeTabKey === key ? " active" : "");
              btn.title = m.path;
              btn.innerHTML = `
                ${iconSvg(m.is_dir, false)}
                <span class="ide-node-name">${esc(m.name)}</span>
                <span class="ide-node-path">${esc(m.path)}</span>
              `;
              btn.addEventListener("click", async () => {
                if (m.is_dir) {
                  searchInput.value = "";
                  searchResults = null;
                  expandedDirs.add(m.path);
                  await loadTreeDir(m.path);
                } else {
                  await openFile(m.path, currentRunId, false);
                }
              });
              frag.appendChild(btn);
            });
            treeEl.appendChild(frag);
            return;
          }

          const rootEntries = treeCache.get("");
          if (!rootEntries) {
            const d = document.createElement("div");
            d.className = "ide-tree-empty";
            d.textContent = "Cargando archivos…";
            treeEl.appendChild(d);
            return;
          }
          if (!rootEntries.length) {
            const d = document.createElement("div");
            d.className = "ide-tree-empty";
            d.textContent = "Carpeta vacía.";
            treeEl.appendChild(d);
            return;
          }
          const frag = document.createDocumentFragment();
          renderTreeNodes("", 0, frag);
          treeEl.appendChild(frag);
        }

        async function refreshTree() {
          const q = searchInput.value.trim();
          if (q) {
            await runSearch(q);
            return;
          }
          const openList = Array.from(expandedDirs);
          treeCache.clear();
          await loadTreeDir("");
          for (const d of openList) {
            await loadTreeDir(d);
          }
        }

        /* --- Pestañas y editor --- */
        async function openFile(path, runId, silent) {
          const key = tabKey(runId, path);
          const existing = tabs.find((t) => t.key === key);
          if (existing) {
            activeTabKey = key;
            renderTabs();
            renderEditor();
            renderTree();
            persistTabs();
            return;
          }
          const params = new URLSearchParams({ path });
          if (runId) params.set("run_id", runId);
          try {
            const r = await apiFetch(`/api/files/read?${params.toString()}`);
            if (!r.ok) {
              if (!silent) {
                const d = await r.json().catch(() => ({ error: `HTTP ${r.status}` }));
                setSaveStatus(d.error || `No se pudo abrir (HTTP ${r.status})`, "err");
              }
              return;
            }
            const data = await r.json();
            const content = typeof data.content === "string" ? data.content : "";
            const tab = {
              key,
              path: data.path || path,
              runId: runId || "",
              content,
              savedContent: content,
              language: data.language || "",
              readOnly: Boolean(data.read_only || runId),
            };
            tabs.push(tab);
            activeTabKey = key;
            setSaveStatus("", "");
            renderTabs();
            renderEditor();
            renderTree();
            persistTabs();
          } catch (_) {
            if (!silent) setSaveStatus("Error de red al leer el archivo", "err");
          }
        }

        function closeTab(key) {
          const idx = tabs.findIndex((t) => t.key === key);
          if (idx < 0) return;
          const tab = tabs[idx];
          if (tab.content !== tab.savedContent) {
            if (!window.confirm(`«${baseName(tab.path)}» tiene cambios sin guardar. ¿Cerrar de todos modos?`)) {
              return;
            }
          }
          tabs.splice(idx, 1);
          if (activeTabKey === key) {
            const next = tabs[idx] || tabs[idx - 1] || null;
            activeTabKey = next ? next.key : null;
          }
          setSaveStatus("", "");
          renderTabs();
          renderEditor();
          renderTree();
          persistTabs();
        }

        function renderTabs() {
          tabsEl.textContent = "";
          tabs.forEach((tab) => {
            const dirty = tab.content !== tab.savedContent;
            const el = document.createElement("div");
            el.className = "ide-tab" + (tab.key === activeTabKey ? " active" : "") + (dirty ? " dirty" : "");
            el.setAttribute("role", "tab");
            el.setAttribute("aria-selected", tab.key === activeTabKey ? "true" : "false");
            el.title = tab.runId ? `${tab.path} (worktree ${tab.runId.slice(0, 8)})` : tab.path;

            const name = document.createElement("span");
            name.className = "ide-tab-name";
            name.textContent = baseName(tab.path);

            const dot = document.createElement("span");
            dot.className = "ide-tab-dirty";
            dot.textContent = "●";
            dot.title = "Modificado";

            const close = document.createElement("button");
            close.type = "button";
            close.className = "ide-tab-close";
            close.textContent = "×";
            close.setAttribute("aria-label", `Cerrar ${baseName(tab.path)}`);
            close.addEventListener("click", (e) => {
              e.stopPropagation();
              closeTab(tab.key);
            });

            el.append(name, dot, close);
            el.addEventListener("click", () => {
              activeTabKey = tab.key;
              setSaveStatus("", "");
              renderTabs();
              renderEditor();
              renderTree();
              persistTabs();
            });
            tabsEl.appendChild(el);
          });
        }

        function updateGutter(text) {
          const count = Math.max(1, String(text || "").split("\n").length);
          if (count === lastLineCount) return;
          lastLineCount = count;
          const lines = new Array(count);
          for (let i = 0; i < count; i += 1) lines[i] = String(i + 1);
          gutterEl.textContent = lines.join("\n");
        }

        function updateToolbarState(tab) {
          if (!tab) {
            activePathEl.textContent = "Sin archivo";
            langEl.style.display = "none";
            roBadgeEl.style.display = "none";
            citeBtn.disabled = true;
            saveBtn.disabled = true;
            return;
          }
          activePathEl.textContent = tab.path;
          activePathEl.title = tab.path;
          if (tab.language) {
            langEl.textContent = tab.language;
            langEl.style.display = "inline-block";
          } else {
            langEl.style.display = "none";
          }
          roBadgeEl.style.display = tab.readOnly ? "inline-block" : "none";
          citeBtn.disabled = false;
          const dirty = tab.content !== tab.savedContent;
          saveBtn.disabled = tab.readOnly || !dirty || saving;
        }

        function renderEditor() {
          const tab = activeTab();
          updateToolbarState(tab);
          if (!tab) {
            emptyEl.style.display = "grid";
            editorBoxEl.style.display = "none";
            editorEl.value = "";
            lastLineCount = 0;
            return;
          }
          emptyEl.style.display = "none";
          editorBoxEl.style.display = "flex";
          if (editorEl.value !== tab.content) {
            editorEl.value = tab.content;
          }
          editorEl.readOnly = tab.readOnly;
          updateGutter(tab.content);
          gutterEl.scrollTop = editorEl.scrollTop;
        }

        async function saveActiveTab() {
          const tab = activeTab();
          if (!tab || tab.readOnly || saving) return;
          if (tab.content === tab.savedContent) {
            setSaveStatus("Sin cambios", "");
            return;
          }
          saving = true;
          updateToolbarState(tab);
          setSaveStatus("Guardando…", "");
          try {
            const r = await apiFetch("/api/files/write", {
              method: "PUT",
              headers: { "content-type": "application/json" },
              body: JSON.stringify({ path: tab.path, content: tab.content }),
            });
            if (!r.ok) {
              const d = await r.json().catch(() => ({ error: `HTTP ${r.status}` }));
              setSaveStatus(d.error || `Error al guardar (HTTP ${r.status})`, "err");
              return;
            }
            tab.savedContent = tab.content;
            setSaveStatus("Guardado", "ok");
            renderTabs();
          } catch (_) {
            setSaveStatus("Error de red al guardar", "err");
          } finally {
            saving = false;
            updateToolbarState(activeTab());
          }
        }

        function citeInOrchestrator() {
          const tab = activeTab();
          if (!tab) return;
          const input = $("orq-input");
          if (!input) return;
          const start = editorEl.selectionStart;
          const end = editorEl.selectionEnd;
          let snippet = "";
          if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
            const before = tab.content.slice(0, start);
            const startLine = before.split("\n").length;
            const endLine = tab.content.slice(0, end).split("\n").length;
            const selected = tab.content.slice(start, end).trim().replace(/\s+/g, " ").slice(0, 180);
            snippet = `\`${tab.path}:${startLine}-${endLine}\` («${selected}»)`;
          } else {
            snippet = `\`${tab.path}\``;
          }
          const prev = input.value.trim();
          input.value = prev ? `${prev} ${snippet} ` : `${snippet} `;
          setView("orquestador");
          input.focus();
          input.setSelectionRange(input.value.length, input.value.length);
        }

        /* --- Origen (Proyecto base / Worktrees) --- */
        function syncSources(runs) {
          const prev = currentRunId;
          const wtRuns = [...(runs || [])]
            .filter((r) => Boolean(r && r.worktree))
            .sort((a, b) => (b.updated_at_ms || 0) - (a.updated_at_ms || 0))
            .slice(0, 25);

          sourceSel.textContent = "";
          const baseOpt = document.createElement("option");
          baseOpt.value = "";
          baseOpt.textContent = "Proyecto base";
          sourceSel.appendChild(baseOpt);

          let hasPrev = !prev;
          wtRuns.forEach((r) => {
            const o = document.createElement("option");
            o.value = r.id;
            const title = (r.task && r.task.titulo) ? r.task.titulo : r.id.slice(0, 8);
            o.textContent = `Worktree · ${title} (${r.id.slice(0, 8)})`;
            if (r.id === prev) hasPrev = true;
            sourceSel.appendChild(o);
          });
          sourceSel.value = hasPrev ? prev : "";
          if (!hasPrev && prev) {
            currentRunId = "";
            treeCache.clear();
            expandedDirs.clear();
            loadTreeDir("");
          }
        }

        /* --- Eventos --- */
        btnOpenWs.addEventListener("click", (e) => {
          e.stopPropagation();
          openWorkspaceModal();
        });
        wsBar.addEventListener("click", () => {
          if ($("app-shell").classList.contains("sidebar-collapsed")) {
            openWorkspaceModal();
          }
        });
        wsCloseBtn.addEventListener("click", closeWorkspaceModal);
        wsModal.addEventListener("click", (e) => {
          if (e.target === wsModal) closeWorkspaceModal();
        });
        wsOpenPathBtn.addEventListener("click", () => selectWorkspace(wsPathInput.value));
        wsPathInput.addEventListener("keydown", (e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            selectWorkspace(wsPathInput.value);
          }
        });
        wsBrowseUp.addEventListener("click", () => {
          if (browseState.parent) browseDir(browseState.parent);
        });
        wsSelectCurrent.addEventListener("click", () => {
          if (browseState.current) selectWorkspace(browseState.current);
        });

        sourceSel.addEventListener("change", () => {
          currentRunId = sourceSel.value || "";
          searchInput.value = "";
          searchResults = null;
          treeCache.clear();
          expandedDirs.clear();
          loadTreeDir("");
        });

        searchInput.addEventListener("input", () => {
          if (searchTimer) clearTimeout(searchTimer);
          const q = searchInput.value;
          searchTimer = setTimeout(() => runSearch(q), 200);
        });

        refreshBtn.addEventListener("click", () => refreshTree());
        saveBtn.addEventListener("click", () => saveActiveTab());
        citeBtn.addEventListener("click", () => citeInOrchestrator());

        editorEl.addEventListener("input", () => {
          const tab = activeTab();
          if (!tab || tab.readOnly) return;
          tab.content = editorEl.value;
          updateGutter(tab.content);
          setSaveStatus("", "");
          updateToolbarState(tab);
          renderTabs();
        });

        editorEl.addEventListener("scroll", () => {
          gutterEl.scrollTop = editorEl.scrollTop;
        });

        editorEl.addEventListener("keydown", (e) => {
          if (e.key === "Tab") {
            const tab = activeTab();
            if (!tab || tab.readOnly) return;
            e.preventDefault();
            const start = editorEl.selectionStart;
            const end = editorEl.selectionEnd;
            const val = editorEl.value;
            editorEl.value = val.slice(0, start) + "  " + val.slice(end);
            editorEl.selectionStart = editorEl.selectionEnd = start + 2;
            tab.content = editorEl.value;
            updateGutter(tab.content);
            setSaveStatus("", "");
            updateToolbarState(tab);
            renderTabs();
          }
        });

        document.addEventListener("keydown", (e) => {
          if (e.key === "Escape" && wsModal.classList.contains("open")) {
            closeWorkspaceModal();
            return;
          }
          if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s" && state.view === "archivos") {
            e.preventDefault();
            saveActiveTab();
          }
        });

        /* --- Ganchos con core.js --- */
        filesApi.onRuns = (runs) => syncSources(runs);
        filesApi.onViewActive = () => {
          if (!activeWorkspace) loadWorkspaces(true);
          if (!treeLoaded) loadTreeDir("");
        };

        /* Carga inicial */
        loadWorkspaces(true).then(() => loadTreeDir(""));
      })();
