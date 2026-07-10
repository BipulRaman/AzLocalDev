// ------------------------------------------------------------- app updates

/** Checks GitHub Releases for a newer version, confirms with the user, and (if accepted)
 * triggers the in-place update + restart. `silent` suppresses the "up to date"/error toasts
 * so the automatic startup check doesn't nag on every launch - the explicit "Check for
 * updates" button always reports its result either way. */
async function checkForUpdates(silent) {
  let result;
  try {
    result = await api("/api/update/check");
  } catch (err) {
    if (!silent) toast("error", `Update check failed: ${err.message || err}`);
    return;
  }

  if (result.status === "available") {
    const go = await confirmDialog(
      `Az.Local.Dev v${result.version} is available. Install it now? The app will restart to finish updating.`,
      { title: "Update available", confirmText: "Update & restart" },
    );
    if (!go) return;
    try {
      toast("success", "Installing update…");
      await api("/api/update/install", { method: "POST" });
    } catch (err) {
      toast("error", `Update failed: ${err.message || err}`);
    }
  } else if (result.status === "up_to_date") {
    if (!silent) toast("success", "You're already on the latest version.");
  } else if (!silent) {
    toast("error", result.error || "Update check failed.");
  }
}

el("check-updates-btn")?.addEventListener("click", () => checkForUpdates(false));
el("about-btn")?.addEventListener("click", () => openModal("modal-about"));

(async function initVersion() {
  try {
    const { version } = await api("/api/version");
    const versionEl = el("app-version");
    // Local dev builds report a valid-but-noisy semver like `0.0.0-dev+3c8fe75` (see
    // `build.rs`'s fallback, needed so `self_update`'s semver comparisons still work) -
    // collapse that down to a plain "dev" for display; released builds (`vX.Y.Z`) show as-is.
    if (versionEl) versionEl.textContent = version.startsWith("0.0.0-dev") ? "dev" : `v${version}`;
  } catch {
    /* ignore - version display is best-effort */
  }
  // Mirrors a typical desktop app's silent startup update check.
  checkForUpdates(true);
})();

// ------------------------------------------------------------ sidebar resize

/** Lets the left nav be widened/narrowed by dragging the thin strip between it and the
 * content column (previously a fixed 240px). Width is persisted to localStorage so it
 * survives reloads/restarts. `.topbar-brand` is kept in sync so the global search box in the
 * top bar keeps lining up with the content column below it (see its own CSS comment). */
(function initSidebarResizer() {
  const sidebar = el("sidebar");
  const resizer = el("sidebar-resizer");
  const brand = el("topbar-brand");
  if (!sidebar || !resizer) return;

  const MIN_WIDTH = 180;
  const MAX_WIDTH = 480;
  const STORAGE_KEY = "sidebarWidth";
  // `.topbar-brand`'s width has to be shorter than the sidebar's by exactly the topbar's own
  // left padding (14px, `.topbar-global`) plus its flex `gap` before the search box (16px) -
  // that's the only way the search box's left edge (padding + brand width + gap) lands
  // exactly on the sidebar's right border below it, instead of drifting 30px too far right.
  const BRAND_WIDTH_OFFSET = 30;

  function applyWidth(px) {
    const width = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, px));
    sidebar.style.width = `${width}px`;
    if (brand) brand.style.width = `${Math.max(0, width - BRAND_WIDTH_OFFSET)}px`;
    return width;
  }

  const saved = parseInt(localStorage.getItem(STORAGE_KEY), 10);
  if (!Number.isNaN(saved)) applyWidth(saved);

  let dragging = false;

  resizer.addEventListener("mousedown", (ev) => {
    dragging = true;
    resizer.classList.add("dragging");
    document.body.classList.add("sidebar-resizing");
    ev.preventDefault();
  });

  window.addEventListener("mousemove", (ev) => {
    if (!dragging) return;
    applyWidth(ev.clientX);
  });

  window.addEventListener("mouseup", () => {
    if (!dragging) return;
    dragging = false;
    resizer.classList.remove("dragging");
    document.body.classList.remove("sidebar-resizing");
    localStorage.setItem(STORAGE_KEY, parseInt(sidebar.style.width, 10));
  });
})();

// ------------------------------------------------------- mobile navigation

(function initMobileNavigation() {
  const menuButton = el("mobile-nav-btn");
  const sidebar = el("sidebar");
  const scrim = el("sidebar-scrim");
  const mobileLayout = window.matchMedia("(max-width: 860px)");
  if (!menuButton || !sidebar || !scrim) return;

  function setOpen(open) {
    const shouldOpen = open && mobileLayout.matches;
    document.body.classList.toggle("sidebar-open", shouldOpen);
    menuButton.setAttribute("aria-expanded", String(shouldOpen));
    menuButton.setAttribute("aria-label", shouldOpen ? "Close navigation" : "Open navigation");
    scrim.tabIndex = shouldOpen ? 0 : -1;
  }

  menuButton.addEventListener("click", () => setOpen(!document.body.classList.contains("sidebar-open")));
  scrim.addEventListener("click", () => {
    setOpen(false);
    menuButton.focus();
  });
  sidebar.addEventListener("click", (event) => {
    if (event.target.closest(".nav-item")) setOpen(false);
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && document.body.classList.contains("sidebar-open")) {
      setOpen(false);
      menuButton.focus();
    }
  });
  mobileLayout.addEventListener("change", () => setOpen(false));
})();


// ----------------------------------------------------------- rename dialog

el("rename-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!renameTarget) return;
  const input = el("rename-name");
  const name = input.value.trim();
  if (!name) return;
  const { kind, id } = renameTarget;
  const path = kind === "group" ? `/api/resource-groups/${id}` : `/api/engines/${id}`;
  try {
    await api(path, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
    toast("success", "Renamed");
    closeModal("modal-rename");
    renameTarget = null;
    await refreshAll();
    if (kind === "group" && currentGroupId === id) {
      currentGroupName = name;
      el("group-title").textContent = name;
    }
    if (kind === "engine" && currentInstanceId === id) {
      currentInstanceName = name;
      el("sb-instance-title").textContent = name;
    }
  } catch (err) {
    toast("error", err.message);
  }
});

// --------------------------------------------------------- global search

function kindDisplayName(kind) {
  const k = kindCache.find((x) => x.kind === kind);
  return k ? k.display_name : kind;
}

/** Shortened label for the left nav only (which has limited width) - e.g. "Storage (Blob +
 * Queue + Table)" just becomes "Storage" there. Every other place (page headings, the "New
 * resource" kind picker, the Type column, etc.) still uses the full `display_name`. */
const KIND_NAV_LABELS = { storage: "Storage" };
function kindNavLabel(k) {
  return KIND_NAV_LABELS[k.kind] || k.display_name;
}

function closeGlobalSearch() {
  el("global-search-results").classList.add("hidden");
  el("global-search-results").innerHTML = "";
}

function renderGlobalSearchResults(query) {
  const results = el("global-search-results");
  const q = query.trim().toLowerCase();
  if (!q) {
    closeGlobalSearch();
    return;
  }

  const items = [];
  groupCache
    .filter((g) => g.name.toLowerCase().includes(q))
    .forEach((g) => items.push({ label: g.name, type: "Resource group", onSelect: () => navGroup(g.id) }));
  engineCache
    .filter((e) => e.display_name.toLowerCase().includes(q))
    .forEach((e) => items.push({ label: e.display_name, type: kindDisplayName(e.kind), onSelect: () => navInstance(e.id) }));

  results.innerHTML = "";
  if (items.length === 0) {
    results.innerHTML = `<div class="search-empty">No matches</div>`;
  } else {
    items.slice(0, 12).forEach((item) => {
      const row = document.createElement("div");
      row.className = "search-result-row";
      row.innerHTML = `<span class="search-result-label"></span><span class="search-result-type"></span>`;
      row.querySelector(".search-result-label").textContent = item.label;
      row.querySelector(".search-result-type").textContent = item.type;
      // mousedown (not click) fires before the input's blur handler would otherwise
      // close the dropdown out from under the click.
      row.addEventListener("mousedown", (ev) => {
        ev.preventDefault();
        item.onSelect();
        el("global-search-input").value = "";
        closeGlobalSearch();
      });
      results.appendChild(row);
    });
  }
  results.classList.remove("hidden");
}

el("global-search-input").addEventListener("input", (ev) => renderGlobalSearchResults(ev.target.value));
el("global-search-input").addEventListener("focus", (ev) => {
  if (ev.target.value.trim()) renderGlobalSearchResults(ev.target.value);
});
el("global-search-input").addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") {
    ev.target.value = "";
    ev.target.blur();
    closeGlobalSearch();
  }
});
document.addEventListener("click", (ev) => {
  if (!el("global-search").contains(ev.target)) closeGlobalSearch();
});

// -------------------------------------------------------------- breadcrumb

function renderBreadcrumbs() {
  const bc = el("breadcrumbs");
  const parts = [{ label: "Home", onClick: navDashboard }];
  if (view === "group" || view === "servicebus" || view === "queue" || view === "storage-blob" || view === "blob-container" || view === "squeue-detail" || view === "stable-detail" || view === "app-insights") {
    parts.push({ label: currentGroupName || currentGroupId, onClick: () => navGroup(currentGroupId) });
  }
  if (view === "servicebus" || view === "queue" || view === "storage-blob" || view === "blob-container" || view === "squeue-detail" || view === "stable-detail" || view === "app-insights") {
    parts.push({ label: currentInstanceName || currentInstanceId, onClick: () => navInstance(currentInstanceId) });
  }
  if (view === "queue") {
    parts.push({ label: currentQueue, onClick: null });
  }
  if (view === "blob-container") {
    parts.push({ label: currentContainerName, onClick: null });
  }
  if (view === "squeue-detail") {
    parts.push({ label: currentSQueueName, onClick: null });
  }
  if (view === "stable-detail") {
    parts.push({ label: currentSTableName, onClick: null });
  }
  if (view === "running") {
    parts.push({ label: "Running resources", onClick: null });
  }
  if (view === "all-resources") {
    parts.push({ label: "Resources", onClick: null });
  }
  if (view === "kind") {
    parts.push({ label: currentKindName || currentKind, onClick: null });
  }

  bc.innerHTML = "";
  parts.forEach((part, i) => {
    if (i > 0) {
      const sep = document.createElement("span");
      sep.className = "sep";
      sep.textContent = "/";
      bc.appendChild(sep);
    }
    if (part.onClick && i < parts.length - 1) {
      const a = document.createElement("a");
      a.textContent = part.label;
      a.addEventListener("click", part.onClick);
      bc.appendChild(a);
    } else {
      const span = document.createElement("span");
      span.className = "current";
      span.textContent = part.label;
      bc.appendChild(span);
    }
  });
}

function showView(name) {
  view = name;
  ["dashboard", "group", "servicebus", "queue", "running", "kind", "all-resources", "storage-blob", "blob-container", "squeue-detail", "stable-detail", "app-insights"].forEach((v) => {
    el(`view-${v}`).classList.toggle("hidden", v !== name);
  });
  el("detail-panel").classList.add("hidden");
  renderBreadcrumbs();
  renderSidebarActiveState();
}

// ------------------------------------------------------------------- nav

function navDashboard() {
  currentGroupId = null;
  currentInstanceId = null;
  showView("dashboard");
  syncUrl();
  refreshAll();
}

function navRunning() {
  currentGroupId = null;
  currentInstanceId = null;
  showView("running");
  syncUrl();
  renderRunningResources();
}

function navAllResources() {
  currentGroupId = null;
  currentInstanceId = null;
  showView("all-resources");
  syncUrl();
  renderAllResources();
}

function navKind(kind) {
  const info = kindCache.find((k) => k.kind === kind);
  currentKind = kind;
  currentKindName = info ? info.display_name : kind;
  currentGroupId = null;
  currentInstanceId = null;
  el("kind-title").textContent = currentKindName;
  showView("kind");
  syncUrl();
  renderKindResources();
}

function navGroup(id) {
  const group = groupCache.find((g) => g.id === id);
  currentGroupId = id;
  currentGroupName = group ? group.name : id;
  el("group-title").textContent = currentGroupName;
  showView("group");
  syncUrl();
  renderGroupResources();
  updateGroupToggle();
}

function navInstance(id) {
  const engine = engineCache.find((e) => e.id === id);
  currentInstanceId = id;
  currentInstanceName = engine ? engine.display_name : id;
  if (engine && !currentGroupId) {
    currentGroupId = engine.group_id;
    const group = groupCache.find((g) => g.id === engine.group_id);
    currentGroupName = group ? group.name : engine.group_id;
  }
  if (engine && engine.kind === "storage") {
    el("blob-instance-title").textContent = currentInstanceName;
    showView("storage-blob");
    showStorageView("containers");
    return;
  }
  if (engine && engine.kind === "app-insights") {
    el("ai-instance-title").textContent = currentInstanceName;
    showView("app-insights");
    showAiView("traces");
    return;
  }
  el("sb-instance-title").textContent = currentInstanceName;
  showView("servicebus");
  syncUrl();
  loadQueues();
}

function navQueue(name) {
  currentQueue = name;
  currentState = "active";
  document.querySelectorAll("#sb-tabs .tab-btn").forEach((b) => b.classList.toggle("active", b.dataset.state === "active"));
  showView("queue");
  el("sb-queue-title").textContent = name;
  syncUrl();
  loadMessages();
}

function navContainer(name) {
  currentContainerName = name;
  showView("blob-container");
  el("blob-container-title").textContent = name;
  syncUrl();
  loadBlobs();
}

/** Switches between the Storage instance's sub-tabs (Containers/Queues/Tables), mirroring
 * the App Insights instance's `showAiView` pattern, and loads that tab's data. */
function showStorageView(name) {
  currentStorageView = name;
  ["containers", "queues", "tables"].forEach((v) => {
    el(`storage-view-${v}`).classList.toggle("hidden", v !== name);
  });
  document.querySelectorAll("#storage-subnav .tab-btn").forEach((b) => b.classList.toggle("active", b.dataset.storageView === name));
  syncUrl();

  if (name === "containers") loadContainers();
  else if (name === "queues") loadSQueues();
  else if (name === "tables") loadSTables();
}

function navSQueue(name) {
  currentSQueueName = name;
  showView("squeue-detail");
  el("squeue-detail-title").textContent = name;
  syncUrl();
  loadSQueueMessages();
}

function navSTable(name) {
  currentSTableName = name;
  showView("stable-detail");
  el("stable-detail-title").textContent = name;
  syncUrl();
  loadSTableEntities();
}

// --------------------------------------------------------------- sidebar

function renderSidebarActiveState() {
  document.querySelectorAll(".nav-item").forEach((a) => {
    const isRunning = a.dataset.nav === "running";
    const isAllResources = a.dataset.nav === "all-resources";
    const isDashboard = a.dataset.nav === "dashboard";
    const isGroup = a.dataset.group && a.dataset.group === currentGroupId;
    const isKind = a.dataset.kind && a.dataset.kind === currentKind;
    const groupActive = isGroup && (view === "group" || view === "servicebus" || view === "queue");
    const dashboardActive = isDashboard && view === "dashboard";
    // Only the section header itself highlights on its own "all resources" overview page -
    // when viewing one specific kind, just that child item should light up, mirroring how
    // "Resource Group" only highlights the specific group (not the section header) when
    // viewing a group's detail page. Previously both highlighted at once here.
    const allResourcesActive = isAllResources && view === "all-resources";
    a.classList.toggle(
      "active",
      (view === "running" && isRunning) || allResourcesActive || (view === "kind" && isKind) || groupActive || dashboardActive
    );
  });
}

/** Whether any/all resources in `groupId` are currently running. */
function groupRunningInfo(groupId) {
  const engines = engineCache.filter((e) => e.group_id === groupId);
  const running = engines.filter((e) => e.running).length;
  return { total: engines.length, running, any: running > 0, all: engines.length > 0 && running === engines.length };
}

function groupName(groupId) {
  const g = groupCache.find((x) => x.id === groupId);
  return g ? g.name : groupId;
}

function renderSidebar() {
  const kindContainer = el("kind-nav");
  kindContainer.innerHTML = "";
  for (const k of kindCache) {
    const ofKind = engineCache.filter((e) => e.kind === k.kind);
    const count = ofKind.length;
    const anyRunning = ofKind.some((e) => e.running);
    const kindLink = document.createElement("a");
    kindLink.href = "#";
    kindLink.className = "nav-item";
    kindLink.dataset.kind = k.kind;
    kindLink.title = `${k.display_name} - ${count} resource${count === 1 ? "" : "s"}, ${ofKind.filter((e) => e.running).length} running`;
    kindLink.innerHTML = `
      <span class="dot ${anyRunning ? "dot-on" : "dot-off"}"></span>
      <span class="nav-item-label">${kindNavLabel(k)}</span>
      <span class="count-chip ${count > 0 ? "has-value" : ""}">${count}</span>
    `;
    kindLink.addEventListener("click", (ev) => {
      ev.preventDefault();
      navKind(k.kind);
    });
    kindContainer.appendChild(kindLink);
  }

  const groupContainer = el("group-nav");
  groupContainer.innerHTML = "";
  for (const group of groupCache) {
    const info = groupRunningInfo(group.id);
    const groupLink = document.createElement("a");
    groupLink.href = "#";
    groupLink.className = "nav-item";
    groupLink.dataset.group = group.id;
    groupLink.title = `${info.total} resource${info.total === 1 ? "" : "s"}, ${info.running} running`;
    groupLink.innerHTML = `
      <span class="dot ${info.any ? "dot-on" : "dot-off"}"></span>
      <span class="nav-item-label">${group.name}</span>
      <span class="count-chip ${info.total > 0 ? "has-value" : ""}">${info.total}</span>
    `;
    groupLink.addEventListener("click", (ev) => {
      ev.preventDefault();
      navGroup(group.id);
    });
    groupContainer.appendChild(groupLink);
  }

  renderSidebarActiveState();
}

