// ------------------------------------------------------------------ state

let view = "dashboard"; // dashboard | group | servicebus | queue | storage-blob | blob-container | squeue-detail | stable-detail | app-insights | running | kind
let groupCache = [];
let engineCache = [];
let kindCache = [];
let currentGroupId = null;
let currentGroupName = "";
let currentInstanceId = null;
let currentInstanceName = "";
let currentQueue = null;
let currentState = "active";
let queueFilter = "";
let currentContainerName = null;
let currentStorageView = "containers"; // containers | queues | tables
let currentSQueueName = null;
let currentSTableName = null;
let currentAiView = "traces"; // traces | trace-detail | logs | metrics | other
let currentTraceOperationId = null;
let currentKind = null;
let currentKindName = "";
let renameTarget = null; // { kind: "group" | "engine", id: string }

// -------------------------------------------------------------- routing
//
// Every top-level page (and most drill-down pages inside an instance) gets its own real URL
// via the History API, so a hard reload / direct link / browser back-forward lands on the
// same page instead of always resetting to the dashboard. `applyLocationRoute()` parses
// `location.pathname` and re-derives the view state from it (used on first load and on
// `popstate`); `syncUrl()` is called by the nav*/show*View functions to push the matching
// URL whenever the user navigates via clicks. `suppressUrlSync` prevents those same nav
// function calls (made *by* `applyLocationRoute()` itself while restoring state) from
// pushing extra history entries.
let suppressUrlSync = false;

function currentPath() {
  if (view === "running") return "/running";
  if (view === "all-resources") return "/resources";
  if (view === "kind" && currentKind) return `/kind/${encodeURIComponent(currentKind)}`;
  if (view === "group" && currentGroupId) return `/group/${encodeURIComponent(currentGroupId)}`;
  if (currentInstanceId && ["servicebus", "queue", "storage-blob", "blob-container", "squeue-detail", "stable-detail", "app-insights"].includes(view)) {
    let path = `/instance/${encodeURIComponent(currentInstanceId)}`;
    if (view === "queue" && currentQueue) path += `/queues/${encodeURIComponent(currentQueue)}`;
    else if (view === "blob-container" && currentContainerName) path += `/containers/${encodeURIComponent(currentContainerName)}`;
    else if (view === "squeue-detail" && currentSQueueName) path += `/queues/${encodeURIComponent(currentSQueueName)}`;
    else if (view === "stable-detail" && currentSTableName) path += `/tables/${encodeURIComponent(currentSTableName)}`;
    else if (view === "storage-blob") path += currentStorageView === "containers" ? "" : `/${currentStorageView}`;
    else if (view === "app-insights") {
      if (currentAiView === "trace-detail" && currentTraceOperationId) path += `/traces/${encodeURIComponent(currentTraceOperationId)}`;
      else if (currentAiView !== "traces") path += `/${currentAiView}`;
    }
    return path;
  }
  return "/";
}

function syncUrl() {
  if (suppressUrlSync) return;
  const path = currentPath();
  if (path !== location.pathname) history.pushState({}, "", path);
}

/** Parses `location.pathname` and restores the app to the matching view - used on first
 * load and whenever the user navigates with the browser's back/forward buttons. Requires
 * `groupCache`/`engineCache`/`kindCache` to already be populated. */
function applyLocationRoute() {
  suppressUrlSync = true;
  try {
    const segments = location.pathname.split("/").filter(Boolean).map((s) => decodeURIComponent(s));
    if (segments.length === 0) {
      navDashboard();
    } else if (segments[0] === "running") {
      navRunning();
    } else if (segments[0] === "resources") {
      navAllResources();
    } else if (segments[0] === "kind" && segments[1]) {
      navKind(segments[1]);
    } else if (segments[0] === "group" && segments[1]) {
      navGroup(segments[1]);
    } else if (segments[0] === "instance" && segments[1]) {
      const id = segments[1];
      navInstance(id);
      const engine = engineCache.find((e) => e.id === id);
      const kind = engine ? engine.kind : null;
      const [, , sub, subName] = segments;
      if (kind === "service-bus" && sub === "queues" && subName) {
        navQueue(subName);
      } else if (kind === "storage") {
        if (sub === "containers" && subName) navContainer(subName);
        else if (sub === "queues" && subName) navSQueue(subName);
        else if (sub === "tables" && subName) navSTable(subName);
        else if (sub === "queues" || sub === "tables" || sub === "containers") showStorageView(sub);
      } else if (kind === "app-insights") {
        if (sub === "traces" && subName) openTrace(subName);
        else if (sub === "logs" || sub === "metrics" || sub === "other") showAiView(sub);
      }
    } else {
      navDashboard();
    }
  } finally {
    suppressUrlSync = false;
  }
}

window.addEventListener("popstate", applyLocationRoute);

const el = (id) => document.getElementById(id);

// -------------------------------------------------------------------- icons

// Small inline icon set shared by every icon-only button in the app, so row actions stay
// compact and visually consistent no matter which table they appear in.
const ICONS = {
  info: '<svg viewBox="0 0 20 20" fill="none"><circle cx="10" cy="10" r="7.25" stroke="currentColor" stroke-width="1.5"/><path d="M10 9v4.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/><circle cx="10" cy="6.6" r="0.95" fill="currentColor"/></svg>',
  trash: '<svg viewBox="0 0 20 20" fill="none"><path d="M4 6h12M8 6V4h4v2M6 6l1 10h6l1-10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  copy: '<svg viewBox="0 0 20 20" fill="none"><rect x="7" y="7" width="10" height="10" rx="1.5" stroke="currentColor" stroke-width="1.5"/><path d="M13 7V4.5A1.5 1.5 0 0 0 11.5 3h-7A1.5 1.5 0 0 0 3 4.5v7A1.5 1.5 0 0 0 4.5 13H7" stroke="currentColor" stroke-width="1.5"/></svg>',
  requeue: '<svg viewBox="0 0 20 20" fill="none"><path d="M4 8a6 6 0 0 1 10.4-4.1M16 4v3.5h-3.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/><path d="M16 12a6 6 0 0 1-10.4 4.1M4 16v-3.5h3.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  edit: '<svg viewBox="0 0 20 20" fill="none"><path d="M12.9 3.9 16.1 7.1M4 16l.7-3.2 8.4-8.4a1.4 1.4 0 0 1 2 0l1.5 1.5a1.4 1.4 0 0 1 0 2L8.2 15.3 4 16Z" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  kebab: '<svg viewBox="0 0 20 20" fill="currentColor"><circle cx="10" cy="4.5" r="1.4"/><circle cx="10" cy="10" r="1.4"/><circle cx="10" cy="15.5" r="1.4"/></svg>',
};

/** Renders a compact, icon-only action button (with a tooltip + accessible label) for use
 * inside table rows, where a full text button would waste horizontal space. */
function iconBtn(icon, title, attrs = "", extraClass = "") {
  return `<button type="button" class="icon-btn-sm ${extraClass}" title="${title}" aria-label="${title}" ${attrs}>${ICONS[icon]}</button>`;
}

// -------------------------------------------------------------------- api

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (!res.ok) {
    let message = `${res.status} ${res.statusText}`;
    try {
      const text = await res.text();
      if (text) message = text;
    } catch {
      /* ignore */
    }
    throw new Error(message);
  }
  const text = await res.text();
  return text ? JSON.parse(text) : null;
}

// ------------------------------------------------------------------ toast

function toast(kind, message) {
  const stack = el("toast-stack");
  const node = document.createElement("div");
  node.className = `toast ${kind}`;
  node.textContent = message;
  stack.appendChild(node);
  setTimeout(() => node.remove(), 3500);
}

// ----------------------------------------------------------------- modals

function openModal(id) {
  const modal = el(id);
  modal.classList.remove("hidden");
  const firstInput = modal.querySelector("input, textarea, select");
  if (firstInput) setTimeout(() => firstInput.focus(), 30);
}

function closeModal(id) {
  el(id).classList.add("hidden");
}

/** Opens the shared rename dialog pre-filled with `currentName`, remembering what's being
 * renamed so the submit handler knows which API endpoint to PATCH. */
function openRenameModal(kind, id, currentName) {
  renameTarget = { kind, id };
  el("rename-title").textContent = kind === "group" ? "Rename resource group" : "Rename resource";
  el("rename-name").value = currentName;
  openModal("modal-rename");
}

document.querySelectorAll("[data-close-modal]").forEach((btn) => {
  btn.addEventListener("click", () => btn.closest(".modal-backdrop").classList.add("hidden"));
});
document.querySelectorAll(".modal-backdrop").forEach((backdrop) => {
  backdrop.addEventListener("click", (ev) => {
    if (ev.target === backdrop) backdrop.classList.add("hidden");
  });
});

// --------------------------------------------------------- confirm dialog

// Replaces the native `confirm()` popup with an in-app modal, so destructive actions look
// and feel consistent with the rest of the dashboard.
let confirmResolve = null;

function confirmDialog(message, opts = {}) {
  el("confirm-title").textContent = opts.title || "Are you sure?";
  el("confirm-message").textContent = message;
  el("confirm-ok-btn").textContent = opts.confirmText || "Delete";
  openModal("modal-confirm");
  return new Promise((resolve) => {
    confirmResolve = resolve;
  });
}

function settleConfirm(result) {
  closeModal("modal-confirm");
  if (confirmResolve) {
    confirmResolve(result);
    confirmResolve = null;
  }
}

el("confirm-ok-btn").addEventListener("click", () => settleConfirm(true));
el("confirm-cancel-btn").addEventListener("click", () => settleConfirm(false));
el("modal-confirm").addEventListener("click", (ev) => {
  if (ev.target.id === "modal-confirm") settleConfirm(false);
});

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

(async function initVersion() {
  try {
    const { version } = await api("/api/version");
    const versionEl = el("app-version");
    if (versionEl) versionEl.textContent = `v${version}`;
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

  function applyWidth(px) {
    const width = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, px));
    sidebar.style.width = `${width}px`;
    if (brand) brand.style.width = `${width}px`;
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
    kindLink.className = "nav-item nav-group-link";
    kindLink.dataset.kind = k.kind;
    kindLink.title = `${count} resource${count === 1 ? "" : "s"}, ${ofKind.filter((e) => e.running).length} running`;
    kindLink.innerHTML = `
      <span class="dot ${anyRunning ? "dot-on" : "dot-off"}"></span>
      <span class="nav-item-label">${k.display_name}</span>
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
    groupLink.className = "nav-item nav-group-link";
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

// -------------------------------------------------------------- home view

async function refreshAll() {
  try {
    [groupCache, engineCache] = await Promise.all([api("/api/resource-groups"), api("/api/engines")]);
  } catch (err) {
    toast("error", `Failed to load resources: ${err.message}`);
    return;
  }
  renderSidebar();
  if (view === "dashboard") renderGroupsTable();
  if (view === "group") renderGroupResources();
  if (view === "all-resources") renderAllResources();
  if (view === "running") renderRunningResources();
  if (view === "kind") renderKindResources();
}

/** Shows/hides the small spinner overlaid on a `.switch` toggle while its resource is being
 * turned on - the actual disabled state is cleared implicitly whenever the row is next
 * re-rendered (from fresh `engineCache`/`groupCache` data), so callers don't need to
 * remember to turn it back off themselves. */
function setSwitchLoading(input, loading) {
  input.disabled = loading;
  const switchEl = input.closest(".switch");
  if (switchEl) switchEl.classList.toggle("loading", loading);
}

/** Starts or stops every resource inside `groupId` in one call. */
async function setGroupRunning(groupId, enable) {
  try {
    await api(`/api/resource-groups/${groupId}/${enable ? "start" : "stop"}`, { method: "POST" });
    toast("success", enable ? "Resource group enabled" : "Resource group disabled");
  } catch (err) {
    toast("error", err.message);
  }
  await refreshAll();
}

function updateGroupToggle() {
  if (!currentGroupId) return;
  const info = groupRunningInfo(currentGroupId);
  const input = el("group-toggle-input");
  const label = el("group-toggle-label");
  input.checked = info.any;
  input.disabled = info.total === 0;
  label.textContent = info.total === 0 ? "No resources" : info.any ? "Enabled" : "Disabled";
}

el("group-toggle-input").addEventListener("change", async (ev) => {
  if (!currentGroupId) return;
  const input = ev.target;
  const enable = input.checked;
  if (enable) setSwitchLoading(input, true);
  else input.disabled = true;
  await setGroupRunning(currentGroupId, enable);
});

function renderGroupsTable() {
  const body = el("groups-body");
  const empty = el("groups-empty");
  const wrap = el("groups");
  body.innerHTML = "";

  if (groupCache.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const g of groupCache) {
    const count = engineCache.filter((e) => e.group_id === g.id).length;
    const info = groupRunningInfo(g.id);
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td class="col-toggle">
        <label class="switch" title="Enable or disable every resource in this group">
          <input type="checkbox" data-group-toggle="${g.id}" data-enabled="${info.any}" ${info.any ? "checked" : ""} ${count === 0 ? "disabled" : ""} />
          <span class="track"></span>
          <span class="switch-spinner"><svg viewBox="0 0 20 20" fill="none"><circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-dasharray="24 100"/></svg></span>
        </label>
      </td>
      <td class="link-cell" data-open-group="${g.id}">${g.name}</td>
      <td>${count} resource${count === 1 ? "" : "s"}</td>
      <td class="mono">${new Date(g.created_at).toLocaleString()}</td>
      <td class="col-actions">
        <div class="row-actions">
          ${iconBtn("edit", "Rename", `data-rename-group="${g.id}"`)}
          ${iconBtn("trash", "Delete", `data-delete-group="${g.id}"`, "icon-btn-danger")}
        </div>
      </td>
    `;
    body.appendChild(tr);
  }

  body.querySelectorAll("[data-open-group]").forEach((cell) => {
    cell.addEventListener("click", () => navGroup(cell.getAttribute("data-open-group")));
  });
  body.querySelectorAll("[data-group-toggle]").forEach((input) => {
    input.addEventListener("change", async (ev) => {
      ev.stopPropagation();
      const id = input.getAttribute("data-group-toggle");
      const enabled = input.getAttribute("data-enabled") === "true";
      if (!enabled) setSwitchLoading(input, true);
      else input.disabled = true;
      await setGroupRunning(id, !enabled);
    });
  });
  body.querySelectorAll("[data-rename-group]").forEach((btn) => {
    btn.addEventListener("click", (ev) => {
      ev.stopPropagation();
      const id = btn.getAttribute("data-rename-group");
      const group = groupCache.find((g) => g.id === id);
      openRenameModal("group", id, group ? group.name : "");
    });
  });
  body.querySelectorAll("[data-delete-group]").forEach((btn) => {
    btn.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      const id = btn.getAttribute("data-delete-group");
      if (!(await confirmDialog("Delete this resource group and everything inside it?"))) return;
      try {
        await api(`/api/resource-groups/${id}`, { method: "DELETE" });
        toast("success", "Resource group deleted");
      } catch (err) {
        toast("error", err.message);
      }
      await refreshAll();
    });
  });
}

// ------------------------------------------------------------- group view

function engineRow(eng) {
  const kindInfo = kindCache.find((k) => k.kind === eng.kind);
  const typeLabel = kindInfo ? kindInfo.display_name : eng.kind;
  const tr = document.createElement("tr");
  tr.innerHTML = `
    <td class="col-toggle">
      <label class="switch" title="${eng.running ? "Stop" : "Start"}">
        <input type="checkbox" data-toggle="${eng.id}" data-running="${eng.running}" ${eng.running ? "checked" : ""} />
        <span class="track"></span>
        <span class="switch-spinner"><svg viewBox="0 0 20 20" fill="none"><circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-dasharray="24 100"/></svg></span>
      </label>
    </td>
    <td class="link-cell" data-open-resource="${eng.id}">${eng.display_name}</td>
    <td>${typeLabel}</td>
    <td class="col-actions">${iconBtn("info", "View details", `data-details="${eng.id}"`)}</td>
    <td class="col-actions">
      <div class="row-actions">
        ${iconBtn("edit", "Rename", `data-rename-engine="${eng.id}"`)}
        ${iconBtn("trash", "Delete", `data-delete-engine="${eng.id}"`, "icon-btn-danger")}
      </div>
    </td>
  `;
  return tr;
}

function wireEngineRowEvents(body) {
  body.querySelectorAll("[data-details]").forEach((btn) => {
    btn.addEventListener("click", (ev) => {
      ev.stopPropagation();
      openDetailsModal(btn.getAttribute("data-details"));
    });
  });
  body.querySelectorAll("[data-rename-engine]").forEach((btn) => {
    btn.addEventListener("click", (ev) => {
      ev.stopPropagation();
      const id = btn.getAttribute("data-rename-engine");
      const eng = engineCache.find((e) => e.id === id);
      openRenameModal("engine", id, eng ? eng.display_name : "");
    });
  });
  body.querySelectorAll("[data-open-resource]").forEach((cell) => {
    cell.addEventListener("click", () => navInstance(cell.getAttribute("data-open-resource")));
  });
  body.querySelectorAll("[data-open-group-cell]").forEach((cell) => {
    cell.addEventListener("click", (ev) => {
      ev.stopPropagation();
      navGroup(cell.getAttribute("data-open-group-cell"));
    });
  });
  body.querySelectorAll("[data-toggle]").forEach((input) => {
    input.addEventListener("change", async (ev) => {
      ev.stopPropagation();
      const id = input.getAttribute("data-toggle");
      const running = input.getAttribute("data-running") === "true";
      if (!running) setSwitchLoading(input, true);
      else input.disabled = true;
      try {
        await api(`/api/engines/${id}/${running ? "stop" : "start"}`, { method: "POST" });
        toast("success", running ? "Resource stopped" : "Resource started");
      } catch (err) {
        toast("error", err.message);
      }
      await refreshAll();
    });
  });
  body.querySelectorAll("[data-delete-engine]").forEach((btn) => {
    btn.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      const id = btn.getAttribute("data-delete-engine");
      if (!(await confirmDialog("Delete this resource? All of its data will be lost."))) return;
      try {
        await api(`/api/engines/${id}`, { method: "DELETE" });
        toast("success", "Resource deleted");
      } catch (err) {
        toast("error", err.message);
      }
      await refreshAll();
    });
  });
}

// ------------------------------------------------------------ resource details

/** Splits a `;`-separated `key=value` connection string (as returned by an engine's
 * `detail`) into an ordered list of fields. Returns an empty array if `detail` doesn't
 * look like a structured connection string, so callers can fall back to showing it as-is. */
function parseConnectionDetails(detail) {
  if (!detail) return [];
  const fields = [];
  for (const part of detail.split(";").map((p) => p.trim()).filter(Boolean)) {
    const eq = part.indexOf("=");
    if (eq <= 0) return [];
    fields.push({ label: part.slice(0, eq), value: part.slice(eq + 1) });
  }
  return fields;
}


/** Renders one labeled field in the details modal, with a copy-to-clipboard icon button. */
function detailsRow(label, value) {
  const text = value ?? "\u2014";
  return `
    <div class="details-row">
      <span class="details-label">${escapeHtml(label)}</span>
      <div class="details-value-row">
        <div class="details-value">${escapeHtml(text)}</div>
        ${iconBtn("copy", "Copy to clipboard", `data-copy-value="${encodeURIComponent(text)}"`)}
      </div>
    </div>
  `;
}

function wireCopyButtons(scope) {
  scope.querySelectorAll("[data-copy-value]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const value = decodeURIComponent(btn.getAttribute("data-copy-value"));
      try {
        await navigator.clipboard.writeText(value);
        toast("success", "Copied to clipboard");
      } catch {
        toast("error", "Could not copy to clipboard");
      }
    });
  });
}

/** Opens a modal with just what's needed to point a local app at this resource for
 * development: the SAS connection string, and (if this resource supports it) the Managed
 * Identity-style `fullyQualifiedNamespace` - each with its own copy button. */
function openDetailsModal(id) {
  const eng = engineCache.find((e) => e.id === id);
  if (!eng) return;
  const kindInfo = kindCache.find((k) => k.kind === eng.kind);
  const typeLabel = kindInfo ? kindInfo.display_name : eng.kind;

  el("details-title").textContent = `${eng.display_name} details`;

  let rows = "";
  rows += detailsRow("Name", eng.display_name);
  rows += detailsRow("Type", typeLabel);
  rows += detailsRow("Resource group", groupName(eng.group_id));
  rows += detailsRow("Status", eng.running ? "Running" : "Stopped");

  const fields = parseConnectionDetails(eng.detail);
  if (fields.length > 0) {
    // Each resource kind that supports a Managed-Identity-style connection, or an
    // alternative ingestion protocol (e.g. App Insights' OTLP endpoint), exposes its own
    // extra field - shown with its own label, alongside the regular connection string.
    const extraFields = [
      { label: "ManagedIdentityNamespace", title: "fullyQualifiedNamespace (Managed Identity)" },
      { label: "ManagedIdentityBlobServiceUri", title: "blobServiceUri (Managed Identity)" },
      { label: "OtlpEndpoint", title: "OTLP endpoint (OTEL_EXPORTER_OTLP_ENDPOINT)" },
      { label: "OtlpProtocol", title: "OTLP protocol (OTEL_EXPORTER_OTLP_PROTOCOL)" },
    ];
    const extraLabels = new Set(extraFields.map((f) => f.label));
    const connectionString = fields
      .filter((f) => !extraLabels.has(f.label))
      .map((f) => `${f.label}=${f.value}`)
      .join(";");
    rows += detailsRow("Connection string", connectionString);
    for (const { label, title } of extraFields) {
      const extraField = fields.find((f) => f.label === label);
      if (extraField) {
        rows += detailsRow(title, extraField.value);
      }
    }
  } else if (eng.detail) {
    rows += detailsRow("Endpoint", eng.detail);
  }

  const body = el("details-body");
  body.innerHTML = rows;
  wireCopyButtons(body);
  openModal("modal-resource-details");
}

function renderGroupResources() {
  if (!currentGroupId) return;
  const body = el("group-resources-body");
  const empty = el("group-resources-empty");
  const wrap = el("group-resources");
  body.innerHTML = "";

  const engines = engineCache.filter((e) => e.group_id === currentGroupId);
  if (engines.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    updateGroupToggle();
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const eng of engines) {
    body.appendChild(engineRow(eng));
  }
  wireEngineRowEvents(body);
  updateGroupToggle();
}

function runningRow(eng) {
  const kindInfo = kindCache.find((k) => k.kind === eng.kind);
  const typeLabel = kindInfo ? kindInfo.display_name : eng.kind;
  const tr = document.createElement("tr");
  tr.innerHTML = `
    <td class="col-toggle">
      <label class="switch" title="Stop">
        <input type="checkbox" data-toggle="${eng.id}" data-running="true" checked />
        <span class="track"></span>
        <span class="switch-spinner"><svg viewBox="0 0 20 20" fill="none"><circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-dasharray="24 100"/></svg></span>
      </label>
    </td>
    <td class="link-cell" data-open-resource="${eng.id}">${eng.display_name}</td>
    <td>${typeLabel}</td>
    <td class="link-cell" data-open-group-cell="${eng.group_id}">${groupName(eng.group_id)}</td>
    <td class="col-actions">${iconBtn("info", "View details", `data-details="${eng.id}"`)}</td>
  `;
  return tr;
}

function kindRow(eng) {
  const tr = document.createElement("tr");
  tr.innerHTML = `
    <td class="col-toggle">
      <label class="switch" title="${eng.running ? "Stop" : "Start"}">
        <input type="checkbox" data-toggle="${eng.id}" data-running="${eng.running}" ${eng.running ? "checked" : ""} />
        <span class="track"></span>
        <span class="switch-spinner"><svg viewBox="0 0 20 20" fill="none"><circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-dasharray="24 100"/></svg></span>
      </label>
    </td>
    <td class="link-cell" data-open-resource="${eng.id}">${eng.display_name}</td>
    <td class="link-cell" data-open-group-cell="${eng.group_id}">${groupName(eng.group_id)}</td>
    <td class="col-actions">${iconBtn("info", "View details", `data-details="${eng.id}"`)}</td>
    <td class="col-actions">
      <div class="row-actions">
        ${iconBtn("edit", "Rename", `data-rename-engine="${eng.id}"`)}
        ${iconBtn("trash", "Delete", `data-delete-engine="${eng.id}"`, "icon-btn-danger")}
      </div>
    </td>
  `;
  return tr;
}

function allResourcesRow(eng) {
  const kindInfo = kindCache.find((k) => k.kind === eng.kind);
  const typeLabel = kindInfo ? kindInfo.display_name : eng.kind;
  const tr = document.createElement("tr");
  tr.innerHTML = `
    <td class="col-toggle">
      <label class="switch" title="${eng.running ? "Stop" : "Start"}">
        <input type="checkbox" data-toggle="${eng.id}" data-running="${eng.running}" ${eng.running ? "checked" : ""} />
        <span class="track"></span>
        <span class="switch-spinner"><svg viewBox="0 0 20 20" fill="none"><circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-dasharray="24 100"/></svg></span>
      </label>
    </td>
    <td class="link-cell" data-open-resource="${eng.id}">${eng.display_name}</td>
    <td>${typeLabel}</td>
    <td class="link-cell" data-open-group-cell="${eng.group_id}">${groupName(eng.group_id)}</td>
    <td class="col-actions">${iconBtn("info", "View details", `data-details="${eng.id}"`)}</td>
    <td class="col-actions">
      <div class="row-actions">
        ${iconBtn("edit", "Rename", `data-rename-engine="${eng.id}"`)}
        ${iconBtn("trash", "Delete", `data-delete-engine="${eng.id}"`, "icon-btn-danger")}
      </div>
    </td>
  `;
  return tr;
}

function renderAllResources() {
  const body = el("all-resources-body");
  const empty = el("all-resources-empty");
  const wrap = el("all-resources");
  body.innerHTML = "";

  if (engineCache.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const eng of engineCache) {
    body.appendChild(allResourcesRow(eng));
  }
  wireEngineRowEvents(body);
}

function renderRunningResources() {
  const body = el("running-resources-body");
  const empty = el("running-resources-empty");
  const wrap = el("running-resources");
  body.innerHTML = "";

  const running = engineCache.filter((e) => e.running);
  if (running.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const eng of running) {
    body.appendChild(runningRow(eng));
  }
  wireEngineRowEvents(body);
}

function renderKindResources() {
  if (!currentKind) return;
  const body = el("kind-resources-body");
  const empty = el("kind-resources-empty");
  const wrap = el("kind-resources");
  body.innerHTML = "";

  const engines = engineCache.filter((e) => e.kind === currentKind);
  if (engines.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const eng of engines) {
    body.appendChild(kindRow(eng));
  }
  wireEngineRowEvents(body);
}

// ------------------------------------------------------------ new group

el("new-group-btn-2").addEventListener("click", () => openModal("modal-new-group"));
el("new-group-btn-3").addEventListener("click", () => openModal("modal-new-group"));

el("new-group-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const input = el("new-group-name");
  const name = input.value.trim();
  if (!name) return;
  try {
    const group = await api("/api/resource-groups", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
    toast("success", `Resource group "${name}" created`);
    input.value = "";
    closeModal("modal-new-group");
    await refreshAll();
    if (group && group.id) navGroup(group.id);
  } catch (err) {
    toast("error", err.message);
  }
});

el("group-delete-btn").addEventListener("click", async () => {
  if (!currentGroupId) return;
  if (!(await confirmDialog("Delete this resource group and everything inside it?"))) return;
  try {
    await api(`/api/resource-groups/${currentGroupId}`, { method: "DELETE" });
    toast("success", "Resource group deleted");
    navDashboard();
  } catch (err) {
    toast("error", err.message);
  }
});

// ---------------------------------------------------------- new container

el("blob-new-container-btn").addEventListener("click", () => openModal("modal-new-container"));
el("blob-new-container-btn-2").addEventListener("click", () => openModal("modal-new-container"));

el("new-container-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!currentInstanceId) return;
  const input = el("new-container-name");
  const name = input.value.trim();
  if (!name) return;
  try {
    await api(`/api/storage-blob/${currentInstanceId}/containers`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
    toast("success", `Container "${name}" created`);
    input.value = "";
    closeModal("modal-new-container");
    await loadContainers();
  } catch (err) {
    toast("error", err.message);
  }
});

el("blob-upload-btn").addEventListener("click", () => el("blob-upload-input").click());

el("blob-upload-input").addEventListener("change", async (ev) => {
  const file = ev.target.files && ev.target.files[0];
  ev.target.value = "";
  if (!file || !currentInstanceId || !currentContainerName) return;
  try {
    await api(
      `/api/storage-blob/${currentInstanceId}/containers/${currentContainerName}/blobs/${encodeURIComponent(file.name)}`,
      {
        method: "PUT",
        headers: { "Content-Type": file.type || "application/octet-stream" },
        body: file,
      }
    );
    toast("success", `Blob "${file.name}" uploaded`);
    await loadBlobs();
  } catch (err) {
    toast("error", err.message);
  }
});

// ------------------------------------------------------------ storage queues/tables

document.querySelectorAll("#storage-subnav .tab-btn").forEach((btn) => {
  btn.addEventListener("click", () => showStorageView(btn.dataset.storageView));
});

el("squeue-new-btn").addEventListener("click", () => openModal("modal-new-squeue"));
el("squeue-new-btn-2").addEventListener("click", () => openModal("modal-new-squeue"));

el("new-squeue-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!currentInstanceId) return;
  const input = el("new-squeue-name");
  const name = input.value.trim();
  if (!name) return;
  try {
    await api(`/api/storage-blob/${currentInstanceId}/queues`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
    toast("success", `Queue "${name}" created`);
    input.value = "";
    closeModal("modal-new-squeue");
    await loadSQueues();
  } catch (err) {
    toast("error", err.message);
  }
});

async function loadSQueues() {
  if (!currentInstanceId) return;
  let queues;
  try {
    queues = await api(`/api/storage-blob/${currentInstanceId}/queues`);
  } catch (err) {
    toast("error", `Failed to load queues: ${err.message}`);
    return;
  }

  const body = el("squeue-list-body");
  const empty = el("squeue-list-empty");
  const wrap = el("squeue-list");
  body.innerHTML = "";

  if (queues.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const q of queues) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td class="link-cell" data-open-squeue="${encodeURIComponent(q.name)}">${escapeHtml(q.name)}</td>
      <td>${q.approximate_message_count}</td>
      <td class="col-actions">${iconBtn("trash", "Delete", `data-delete-squeue="${encodeURIComponent(q.name)}"`, "icon-btn-danger")}</td>
    `;
    tr.querySelector("[data-open-squeue]").addEventListener("click", () => navSQueue(q.name));
    tr.querySelector("[data-delete-squeue]").addEventListener("click", async (ev) => {
      ev.stopPropagation();
      if (!(await confirmDialog(`Delete queue "${q.name}"? All of its messages will be lost.`))) return;
      try {
        await api(`/api/storage-blob/${currentInstanceId}/queues/${q.name}`, { method: "DELETE" });
        toast("success", `Queue "${q.name}" deleted`);
      } catch (err) {
        toast("error", err.message);
      }
      await loadSQueues();
    });
    body.appendChild(tr);
  }
}

async function loadSQueueMessages() {
  if (!currentInstanceId || !currentSQueueName) return;
  let messages;
  try {
    messages = await api(`/api/storage-blob/${currentInstanceId}/queues/${currentSQueueName}/messages`);
  } catch (err) {
    toast("error", `Failed to load messages: ${err.message}`);
    return;
  }

  const body = el("squeue-messages-body");
  const empty = el("squeue-messages-empty");
  const wrap = el("squeue-messages");
  body.innerHTML = "";

  if (messages.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const m of messages) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td class="mono">${escapeHtml(m.message_id)}</td>
      <td class="mono">${new Date(m.insertion_time).toLocaleString()}</td>
      <td class="mono">${new Date(m.expiration_time).toLocaleString()}</td>
      <td>${m.dequeue_count}</td>
      <td class="body-cell">${escapeHtml(m.body)}</td>
    `;
    body.appendChild(tr);
  }
}

el("squeue-send-btn").addEventListener("click", () => openModal("modal-squeue-send"));

el("squeue-send-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!currentInstanceId || !currentSQueueName) return;
  const input = el("squeue-send-body");
  const body = input.value;
  try {
    await api(`/api/storage-blob/${currentInstanceId}/queues/${currentSQueueName}/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ body }),
    });
    toast("success", "Message sent");
    input.value = "";
    closeModal("modal-squeue-send");
    await loadSQueueMessages();
  } catch (err) {
    toast("error", err.message);
  }
});

el("squeue-clear-btn").addEventListener("click", async () => {
  if (!currentInstanceId || !currentSQueueName) return;
  if (!(await confirmDialog(`Clear all messages from "${currentSQueueName}"?`, { confirmText: "Clear" }))) return;
  try {
    await api(`/api/storage-blob/${currentInstanceId}/queues/${currentSQueueName}/messages`, { method: "DELETE" });
    toast("success", "Queue cleared");
  } catch (err) {
    toast("error", err.message);
  }
  await loadSQueueMessages();
});

el("stable-new-btn").addEventListener("click", () => openModal("modal-new-stable"));
el("stable-new-btn-2").addEventListener("click", () => openModal("modal-new-stable"));

el("new-stable-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!currentInstanceId) return;
  const input = el("new-stable-name");
  const name = input.value.trim();
  if (!name) return;
  try {
    await api(`/api/storage-blob/${currentInstanceId}/tables`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
    toast("success", `Table "${name}" created`);
    input.value = "";
    closeModal("modal-new-stable");
    await loadSTables();
  } catch (err) {
    toast("error", err.message);
  }
});

async function loadSTables() {
  if (!currentInstanceId) return;
  let tables;
  try {
    tables = await api(`/api/storage-blob/${currentInstanceId}/tables`);
  } catch (err) {
    toast("error", `Failed to load tables: ${err.message}`);
    return;
  }

  const body = el("stable-list-body");
  const empty = el("stable-list-empty");
  const wrap = el("stable-list");
  body.innerHTML = "";

  if (tables.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const t of tables) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td class="link-cell" data-open-stable="${encodeURIComponent(t.name)}">${escapeHtml(t.name)}</td>
      <td>${t.entity_count}</td>
      <td class="col-actions">${iconBtn("trash", "Delete", `data-delete-stable="${encodeURIComponent(t.name)}"`, "icon-btn-danger")}</td>
    `;
    tr.querySelector("[data-open-stable]").addEventListener("click", () => navSTable(t.name));
    tr.querySelector("[data-delete-stable]").addEventListener("click", async (ev) => {
      ev.stopPropagation();
      if (!(await confirmDialog(`Delete table "${t.name}"? All of its entities will be lost.`))) return;
      try {
        await api(`/api/storage-blob/${currentInstanceId}/tables/${t.name}`, { method: "DELETE" });
        toast("success", `Table "${t.name}" deleted`);
      } catch (err) {
        toast("error", err.message);
      }
      await loadSTables();
    });
    body.appendChild(tr);
  }
}

async function loadSTableEntities() {
  if (!currentInstanceId || !currentSTableName) return;
  let entities;
  try {
    entities = await api(`/api/storage-blob/${currentInstanceId}/tables/${currentSTableName}/entities`);
  } catch (err) {
    toast("error", `Failed to load entities: ${err.message}`);
    return;
  }

  const body = el("stable-entities-body");
  const empty = el("stable-entities-empty");
  const wrap = el("stable-entities");
  body.innerHTML = "";

  if (entities.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const e of entities) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td class="mono">${escapeHtml(e.partition_key)}</td>
      <td class="mono">${escapeHtml(e.row_key)}</td>
      <td class="mono">${new Date(e.timestamp).toLocaleString()}</td>
      <td class="body-cell">${escapeHtml(JSON.stringify(e.properties))}</td>
      <td class="col-actions">${iconBtn("trash", "Delete", `data-delete-entity="${encodeURIComponent(e.partition_key)}|${encodeURIComponent(e.row_key)}"`, "icon-btn-danger")}</td>
    `;
    tr.querySelector("[data-delete-entity]").addEventListener("click", async () => {
      if (!(await confirmDialog("Delete this entity?"))) return;
      try {
        await api(`/api/storage-blob/${currentInstanceId}/tables/${currentSTableName}/entities/${encodeURIComponent(e.partition_key)}/${encodeURIComponent(e.row_key)}`, {
          method: "DELETE",
        });
        toast("success", "Entity deleted");
      } catch (err) {
        toast("error", err.message);
      }
      await loadSTableEntities();
    });
    body.appendChild(tr);
  }
}

el("stable-insert-btn").addEventListener("click", () => openModal("modal-stable-insert"));

el("stable-insert-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!currentInstanceId || !currentSTableName) return;
  const partitionKey = el("stable-insert-pk").value.trim();
  const rowKey = el("stable-insert-rk").value.trim();
  const propsText = el("stable-insert-props").value.trim();
  let properties = {};
  if (propsText) {
    try {
      properties = JSON.parse(propsText);
    } catch {
      toast("error", "Properties must be valid JSON");
      return;
    }
  }
  try {
    await api(`/api/storage-blob/${currentInstanceId}/tables/${currentSTableName}/entities`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ partition_key: partitionKey, row_key: rowKey, properties }),
    });
    toast("success", "Entity inserted");
    el("stable-insert-pk").value = "";
    el("stable-insert-rk").value = "";
    el("stable-insert-props").value = "";
    closeModal("modal-stable-insert");
    await loadSTableEntities();
  } catch (err) {
    toast("error", err.message);
  }
});

// ------------------------------------------------------------ new resource

async function loadResourceKinds() {
  try {
    kindCache = await api("/api/resource-kinds");
  } catch (err) {
    toast("error", `Failed to load resource types: ${err.message}`);
    return;
  }
  const select = el("new-resource-kind");
  select.innerHTML = kindCache
    .map((k) => `<option value="${k.kind}">${k.display_name}</option>`)
    .join("");
}

function populateGroupSelect(preselectId) {
  const select = el("new-resource-group");
  select.innerHTML = groupCache.map((g) => `<option value="${g.id}">${g.name}</option>`).join("");
  if (preselectId) select.value = preselectId;
}

async function openNewResourceModal(preselectGroupId) {
  await loadResourceKinds();
  populateGroupSelect(preselectGroupId);
  openModal("modal-new-resource");
}

el("new-resource-btn").addEventListener("click", () => openNewResourceModal(currentGroupId));
el("group-new-resource-btn").addEventListener("click", () => openNewResourceModal(currentGroupId));
el("group-new-resource-btn-2").addEventListener("click", () => openNewResourceModal(currentGroupId));

el("new-resource-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const kind = el("new-resource-kind").value;
  const groupId = el("new-resource-group").value;
  const nameInput = el("new-resource-name");
  const name = nameInput.value.trim();
  if (!name || !kind || !groupId) return;
  try {
    const engine = await api("/api/engines", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ kind, name, group_id: groupId }),
    });
    toast("success", `"${name}" created`);
    nameInput.value = "";
    closeModal("modal-new-resource");
    await refreshAll();
    if (engine && engine.id) navInstance(engine.id);
  } catch (err) {
    toast("error", err.message);
  }
});

// ----------------------------------------------------------- service bus

function countChip(label, value, opts = {}) {
  const cls = ["count-chip"];
  if (value > 0) cls.push("has-value");
  if (opts.dlq) cls.push("dlq");
  return `<span class="${cls.join(" ")}">${label} ${value}</span>`;
}

async function loadQueues() {
  if (!currentInstanceId) return;
  let queues;
  try {
    queues = await api(`/api/service-bus/${currentInstanceId}/queues`);
  } catch (err) {
    toast("error", `Failed to load queues: ${err.message}`);
    return;
  }

  if (queueFilter) {
    queues = queues.filter((q) => q.name.toLowerCase().includes(queueFilter.toLowerCase()));
  }

  const body = el("sb-queues-body");
  const empty = el("sb-queues-empty");
  const wrap = el("sb-queues");
  body.innerHTML = "";

  if (queues.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const q of queues) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td class="link-cell" data-open="${q.name}">${q.name}</td>
      <td>${countChip("", q.stats.active_count)}</td>
      <td>${countChip("", q.stats.scheduled_count)}</td>
      <td>${countChip("", q.stats.deferred_count)}</td>
      <td>${countChip("", q.stats.dead_letter_count, { dlq: true })}</td>
      <td class="col-actions">
        <div class="row-actions">
          ${iconBtn("trash", "Delete", `data-delete="${q.name}"`, "icon-btn-danger")}
        </div>
      </td>
    `;
    body.appendChild(tr);
  }

  body.querySelectorAll("[data-open]").forEach((elm) => {
    elm.addEventListener("click", () => navQueue(elm.getAttribute("data-open")));
  });
  body.querySelectorAll("[data-delete]").forEach((btn) => {
    btn.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      const name = btn.getAttribute("data-delete");
      if (!(await confirmDialog(`Delete queue "${name}"? This cannot be undone.`))) return;
      try {
        await api(`/api/service-bus/${currentInstanceId}/queues/${name}`, { method: "DELETE" });
        toast("success", `Queue "${name}" deleted`);
      } catch (err) {
        toast("error", err.message);
      }
      await loadQueues();
    });
  });
}

async function loadMessages() {
  if (!currentInstanceId || !currentQueue) return;
  let rows;
  try {
    rows = await api(
      `/api/service-bus/${currentInstanceId}/queues/${currentQueue}/messages?state=${currentState}&from=1&count=200`
    );
  } catch (err) {
    toast("error", `Failed to load messages: ${err.message}`);
    return;
  }

  const body = document.querySelector("#sb-messages-table tbody");
  const empty = el("sb-messages-empty");
  const table = el("sb-messages-table");
  body.innerHTML = "";

  if (rows.length === 0) {
    empty.classList.remove("hidden");
    table.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  table.classList.remove("hidden");

  for (const m of rows) {
    const tr = document.createElement("tr");
    const actions = currentState === "deadlettered"
      ? iconBtn("requeue", "Move to queue", `data-resubmit-msg="${m.sequence_number}"`) +
        iconBtn("trash", "Delete", `data-delete-msg="${m.sequence_number}"`, "icon-btn-danger")
      : iconBtn("trash", "Delete", `data-delete-msg="${m.sequence_number}"`, "icon-btn-danger");
    tr.innerHTML = `
      <td class="mono">${m.sequence_number}</td>
      <td class="mono">${m.message_id}</td>
      <td class="mono">${m.session_id ? escapeHtml(m.session_id) : "—"}</td>
      <td class="mono">${new Date(m.enqueued_time).toLocaleString()}</td>
      <td>${m.delivery_count}</td>
      <td class="body-cell">${escapeHtml(m.body_text)}</td>
      <td class="col-actions"><div class="row-actions">${actions}</div></td>
    `;
    body.appendChild(tr);
  }

  body.querySelectorAll("[data-delete-msg]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const seq = btn.getAttribute("data-delete-msg");
      if (!(await confirmDialog(`Delete message #${seq}? This cannot be undone.`))) return;
      try {
        await api(`/api/service-bus/${currentInstanceId}/queues/${currentQueue}/messages/${seq}`, {
          method: "DELETE",
        });
        toast("success", `Message #${seq} deleted`);
      } catch (err) {
        toast("error", err.message);
      }
      await loadMessages();
    });
  });

  body.querySelectorAll("[data-resubmit-msg]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const seq = btn.getAttribute("data-resubmit-msg");
      if (!(await confirmDialog(`Move message #${seq} back into the queue as a fresh message?`, { confirmText: "Move" }))) return;
      try {
        const res = await api(
          `/api/service-bus/${currentInstanceId}/queues/${currentQueue}/messages/${seq}/resubmit`,
          { method: "POST" }
        );
        toast("success", `Message #${seq} moved to queue as #${res.sequence_number}`);
      } catch (err) {
        toast("error", err.message);
      }
      await loadMessages();
    });
  });
}

function escapeHtml(str) {
  const div = document.createElement("div");
  div.textContent = str;
  return div.innerHTML;
}

// -------------------------------------------------------- storage (blob)

function formatBytes(n) {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = n / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[i]}`;
}

async function loadContainers() {
  if (!currentInstanceId) return;
  let containers;
  try {
    containers = await api(`/api/storage-blob/${currentInstanceId}/containers`);
  } catch (err) {
    toast("error", `Failed to load containers: ${err.message}`);
    return;
  }

  const body = el("blob-containers-body");
  const empty = el("blob-containers-empty");
  const wrap = el("blob-containers");
  body.innerHTML = "";

  if (containers.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const c of containers) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td class="link-cell" data-open-container="${c.name}">${escapeHtml(c.name)}</td>
      <td>${c.blob_count}</td>
      <td class="mono">${new Date(c.created_at).toLocaleString()}</td>
      <td class="col-actions">
        <div class="row-actions">
          ${iconBtn("trash", "Delete", `data-delete-container="${c.name}"`, "icon-btn-danger")}
        </div>
      </td>
    `;
    body.appendChild(tr);
  }

  body.querySelectorAll("[data-open-container]").forEach((elm) => {
    elm.addEventListener("click", () => navContainer(elm.getAttribute("data-open-container")));
  });
  body.querySelectorAll("[data-delete-container]").forEach((btn) => {
    btn.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      const name = btn.getAttribute("data-delete-container");
      if (!(await confirmDialog(`Delete container "${name}"? All of its blobs will be lost.`))) return;
      try {
        await api(`/api/storage-blob/${currentInstanceId}/containers/${name}`, { method: "DELETE" });
        toast("success", `Container "${name}" deleted`);
      } catch (err) {
        toast("error", err.message);
      }
      await loadContainers();
    });
  });
}

async function loadBlobs() {
  if (!currentInstanceId || !currentContainerName) return;
  let blobs;
  try {
    blobs = await api(
      `/api/storage-blob/${currentInstanceId}/containers/${currentContainerName}/blobs`
    );
  } catch (err) {
    toast("error", `Failed to load blobs: ${err.message}`);
    return;
  }

  const body = el("blob-blobs-body");
  const empty = el("blob-blobs-empty");
  const wrap = el("blob-blobs");
  body.innerHTML = "";

  if (blobs.length === 0) {
    empty.classList.remove("hidden");
    wrap.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  wrap.classList.remove("hidden");

  for (const b of blobs) {
    const downloadUrl = `/api/storage-blob/${currentInstanceId}/containers/${currentContainerName}/blobs/${encodeURIComponent(b.name)}`;
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td><a class="mono" href="${downloadUrl}" target="_blank" rel="noopener">${escapeHtml(b.name)}</a></td>
      <td>${formatBytes(b.content_length)}</td>
      <td class="mono">${escapeHtml(b.content_type)}</td>
      <td class="mono">${new Date(b.last_modified).toLocaleString()}</td>
      <td class="col-actions">
        <div class="row-actions">
          ${iconBtn("trash", "Delete", `data-delete-blob="${encodeURIComponent(b.name)}"`, "icon-btn-danger")}
        </div>
      </td>
    `;
    body.appendChild(tr);
  }

  body.querySelectorAll("[data-delete-blob]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const name = decodeURIComponent(btn.getAttribute("data-delete-blob"));
      if (!(await confirmDialog(`Delete blob "${name}"? This cannot be undone.`))) return;
      try {
        await api(`/api/storage-blob/${currentInstanceId}/containers/${currentContainerName}/blobs/${encodeURIComponent(name)}`, {
          method: "DELETE",
        });
        toast("success", `Blob "${name}" deleted`);
      } catch (err) {
        toast("error", err.message);
      }
      await loadBlobs();
    });
  });
}

// ------------------------------------------------------- app insights

// Application Insights' `SeverityLevel` only has 5 values (unlike ILogger's 6-value
// `LogLevel`) - the ApplicationInsightsLoggerProvider maps both `LogLevel.Trace` and
// `LogLevel.Debug` down to `SeverityLevel.Verbose`, so "Trace" is the closest equivalent
// label for level 0 here.
const SEVERITY_LABELS = ["Trace", "Information", "Warning", "Error", "Critical"];
const SEVERITY_PILL_CLASS = ["pill-muted", "pill-on", "pill-warning", "pill-danger", "pill-danger"];

/** Renders `ms` (milliseconds, possibly fractional) the way the rest of the dashboard
 * formats durations - `—` when absent, otherwise a compact "Nms"/"Ns" label. */
function formatDurationMs(ms) {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

function severityPill(level) {
  const label = SEVERITY_LABELS[level] ?? "Information";
  const cls = SEVERITY_PILL_CLASS[level] ?? "pill-on";
  return `<span class="pill ${cls}">${label}</span>`;
}

/** Renders a telemetry item's "Trace" column: a link to the distributed operation it
 * belongs to, or a dash if it was never correlated to one. */
function traceCell(item) {
  if (!item.operation_id) return "—";
  const shortId = item.operation_id.length > 12 ? `${item.operation_id.slice(0, 12)}…` : item.operation_id;
  return `<span class="link-cell" data-open-trace-cell="${encodeURIComponent(item.operation_id)}">${escapeHtml(shortId)}</span>`;
}

function wireTraceCells(scope) {
  scope.querySelectorAll("[data-open-trace-cell]").forEach((elm) => {
    elm.addEventListener("click", (ev) => {
      ev.stopPropagation();
      openTrace(decodeURIComponent(elm.getAttribute("data-open-trace-cell")));
    });
  });
}

function formatPropValue(v) {
  if (v === null || v === undefined || v === "") return "—";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/** Renders one Name/Value row for the details panel. Values are always shown in full
 * (wrapped, not truncated) - opening the panel is meant to be a single step that shows
 * everything at once, with no extra click needed to reveal a truncated field. */
function kvRow(name, rawValue) {
  const text = formatPropValue(rawValue);
  return `<tr><td>${escapeHtml(name)}</td><td class="kv-value">${escapeHtml(text)}</td></tr>`;
}

/** Renders one name/value section of the details panel (Aspire-dashboard-style "Log
 * entry"/"Context"/"Resource" groups) - `rows` is an array of `[name, value]` pairs.
 * Returns an empty string (renders nothing) if every row's value is undefined. Sections are
 * always expanded by default - the header toggle is just a convenience to collapse a
 * section you're not interested in, never required to see the rest. */
function kvSection(title, rows) {
  const visible = rows.filter(([, v]) => v !== undefined);
  if (visible.length === 0) return "";
  const body = visible.map(([name, value]) => kvRow(name, value)).join("");
  return `
    <div class="detail-section">
      <div class="detail-section-header" data-toggle-section>
        <span>${escapeHtml(title)}</span>
        <span class="count-chip has-value">${visible.length}</span>
        <svg viewBox="0 0 20 20" fill="none"><path d="m6 8 4 4 4-4" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>
      </div>
      <table><thead><tr><th>Name</th><th>Value</th></tr></thead><tbody>${body}</tbody></table>
    </div>
  `;
}


/** Human-readable panel title per telemetry type, matching the Aspire dashboard's generic
 * "Log entry details" panel title (as opposed to repeating the message text, which already
 * has its own "Message" row in the "Log entry" section below). */
const PANEL_TITLES = {
  request: "Request details",
  dependency: "Dependency details",
  exception: "Exception details",
  trace: "Log entry details",
  event: "Event details",
  metric: "Metric details",
  page_view: "Page view details",
  availability: "Availability details",
  other: "Telemetry details",
};

/** Opens the Aspire-dashboard-style details flyout for one telemetry item: grouped
 * "Log entry"/"Context"/"Resource" name-value tables (everything fully expanded, nothing
 * truncated), plus a "Raw JSON" section - a single click is meant to show everything, with
 * no second click needed to reveal any field. Shared by every App Insights sub-view's row
 * action. */
function openDetailPanel(item) {
  const tags = item.tags || {};
  const resourceLabel = tags["ai.cloud.role"] || tags["ai.cloud.roleInstance"] || (item.ikey ? item.ikey.slice(0, 8) : "—");
  const category = item.operation_name || item.item_type;

  el("detail-panel-title").textContent = PANEL_TITLES[item.item_type] || "Telemetry details";
  el("detail-panel-subtitle").innerHTML = `
    <span><strong>Category</strong> ${escapeHtml(category)}</span>
    <span><strong>Resource</strong> ${escapeHtml(resourceLabel)}</span>
    <span><strong>Timestamp</strong> ${escapeHtml(new Date(item.time || item.received_at).toLocaleString())}</span>
  `;

  const entryRows = [];
  if (item.severity !== null && item.severity !== undefined) entryRows.push(["Level", SEVERITY_LABELS[item.severity] ?? item.severity]);
  entryRows.push(["Message", item.name]);
  if (item.duration_ms !== null && item.duration_ms !== undefined) entryRows.push(["Duration", formatDurationMs(item.duration_ms)]);
  if (item.response_code) entryRows.push(["Response code", item.response_code]);
  if (item.success !== null && item.success !== undefined) entryRows.push(["Success", item.success ? "True" : "False"]);
  for (const [key, value] of Object.entries(item.properties || {})) {
    entryRows.push([key, value]);
  }

  const contextRows = [
    ["Category", item.operation_name || "—"],
    ["Item type", item.item_type],
    ["Operation Id", item.operation_id || "—"],
  ];

  const resourceRows = [
    ["Instrumentation key", item.ikey || "—"],
    ["Cloud role", tags["ai.cloud.role"] || "—"],
    ["Role instance", tags["ai.cloud.roleInstance"] || "—"],
    ["SDK version", tags["ai.internal.sdkVersion"] || "—"],
  ];

  const body = el("detail-panel-body");
  body.innerHTML =
    kvSection("Log entry", entryRows) +
    kvSection("Context", contextRows) +
    kvSection("Resource", resourceRows) +
    `<div class="detail-section">
      <div class="detail-section-header" data-toggle-section>
        <span>Raw JSON</span>
        <svg viewBox="0 0 20 20" fill="none"><path d="m6 8 4 4 4-4" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>
      </div>
      <pre class="details-json">${escapeHtml(JSON.stringify(item, null, 2))}</pre>
    </div>`;

  body.querySelectorAll("[data-toggle-section]").forEach((header) => {
    header.addEventListener("click", () => header.closest(".detail-section").classList.toggle("collapsed"));
  });

  el("detail-panel").classList.remove("hidden");
}

el("detail-panel-close").addEventListener("click", () => el("detail-panel").classList.add("hidden"));

/** Switches between the App Insights instance's sub-views (Traces / Trace detail /
 * Structured Logs / Metrics / Other), mirroring the Aspire dashboard's tab layout, and
 * loads that view's data. */
function showAiView(name) {
  currentAiView = name;
  ["traces", "trace-detail", "logs", "metrics", "other"].forEach((v) => {
    el(`ai-view-${v}`).classList.toggle("hidden", v !== name);
  });
  document.querySelectorAll("#ai-subnav .tab-btn").forEach((b) => b.classList.toggle("active", b.dataset.aiView === name));
  el("detail-panel").classList.add("hidden");
  syncUrl();

  if (name === "traces") loadTraces();
  else if (name === "logs") loadLogs();
  else if (name === "metrics") loadMetrics();
  else if (name === "other") loadOther();
}

function openTrace(operationId) {
  currentTraceOperationId = operationId;
  showAiView("trace-detail");
  loadTraceDetail();
}

/** Opens a trace row: if it has no request/dependency spans (a standalone log/exception
 * with nothing to waterfall), the trace-detail page would just be an empty waterfall with
 * a single log row underneath - so this skips straight to the details panel for that item
 * instead of navigating to that mostly-empty page. */
async function openTraceRow(t) {
  if (t.span_count > 0) {
    openTrace(t.operation_id);
    return;
  }
  let items;
  try {
    items = await api(`/api/app-insights/${currentInstanceId}/traces/${encodeURIComponent(t.operation_id)}`);
  } catch (err) {
    toast("error", `Failed to load trace: ${err.message}`);
    return;
  }
  if (items.length > 0) {
    openDetailPanel(items[0]);
  }
}

async function loadTraces() {
  if (!currentInstanceId) return;
  let traces;
  try {
    traces = await api(`/api/app-insights/${currentInstanceId}/traces`);
  } catch (err) {
    toast("error", `Failed to load traces: ${err.message}`);
    return;
  }

  const table = el("ai-traces-table");
  const body = table.querySelector("tbody");
  const empty = el("ai-traces-empty");
  body.innerHTML = "";

  if (traces.length === 0) {
    empty.classList.remove("hidden");
    table.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  table.classList.remove("hidden");

  const maxDuration = Math.max(...traces.map((t) => t.duration_ms), 1);
  for (const t of traces) {
    const tr = document.createElement("tr");
    const pct = Math.max((t.duration_ms / maxDuration) * 100, 2);
    tr.innerHTML = `
      <td class="mono">${new Date(t.start_time).toLocaleString()}</td>
      <td class="link-cell" data-open-trace="${encodeURIComponent(t.operation_id)}">
        ${t.has_error ? '<span class="error-dot"></span>' : ""}${escapeHtml(t.root_name)}
      </td>
      <td>${t.span_count}</td>
      <td>
        <div class="duration-cell">
          <div class="duration-bar-track"><div class="duration-bar-fill${t.has_error ? " error" : ""}" style="width:${pct}%"></div></div>
          <span class="mono">${formatDurationMs(t.duration_ms)}</span>
        </div>
      </td>
      <td class="col-actions"></td>
    `;
    tr.querySelector("[data-open-trace]").addEventListener("click", () => openTraceRow(t));
    body.appendChild(tr);
  }
}

el("ai-back-to-traces-btn").addEventListener("click", () => showAiView("traces"));

async function loadTraceDetail() {
  if (!currentInstanceId || !currentTraceOperationId) return;
  let items;
  try {
    items = await api(`/api/app-insights/${currentInstanceId}/traces/${encodeURIComponent(currentTraceOperationId)}`);
  } catch (err) {
    toast("error", `Failed to load trace: ${err.message}`);
    return;
  }

  const spans = items.filter((i) => i.item_type === "request" || i.item_type === "dependency");
  const events = items.filter((i) => i.item_type === "trace" || i.item_type === "exception");
  const hasError = events.some((i) => i.item_type === "exception") || spans.some((s) => s.success === false);

  const startedAt = (i) => new Date(i.time || i.received_at).getTime();
  const traceStart = Math.min(...items.map(startedAt));
  const traceEnd = Math.max(...items.map((i) => startedAt(i) + (i.duration_ms || 0)));
  const totalDuration = Math.max(traceEnd - traceStart, 1);
  const root = spans.find((s) => s.item_type === "request") || spans[0];
  const rootName = root ? root.name : (items[0] && (items[0].operation_id || "Trace"));

  el("ai-trace-summary").innerHTML = `
    <div class="stat"><span class="stat-label">Name</span><span class="stat-value">${escapeHtml(rootName || "")}</span></div>
    <div class="stat"><span class="stat-label">Start time</span><span class="stat-value">${new Date(traceStart).toLocaleString()}</span></div>
    <div class="stat"><span class="stat-label">Duration</span><span class="stat-value">${formatDurationMs(totalDuration)}</span></div>
    <div class="stat"><span class="stat-label">Spans</span><span class="stat-value">${spans.length}</span></div>
    <div class="stat"><span class="stat-label">Status</span><span class="stat-value${hasError ? " error" : ""}">${hasError ? "Error" : "Success"}</span></div>
  `;

  const waterfall = el("ai-waterfall");
  waterfall.className = "waterfall";
  waterfall.innerHTML = "";
  // Root spans (no parent found within this trace) first, then everything else in start-time
  // order - approximates a depth-first waterfall without needing a full tree structure.
  const bySpanId = new Map(spans.filter((s) => s.span_id).map((s) => [s.span_id, s]));
  const depthOf = (span, seen = new Set()) => {
    if (!span.parent_span_id || seen.has(span.span_id)) return 0;
    const parent = bySpanId.get(span.parent_span_id);
    if (!parent) return 0;
    seen.add(span.span_id);
    return 1 + depthOf(parent, seen);
  };
  const sortedSpans = [...spans].sort((a, b) => startedAt(a) - startedAt(b));
  for (const span of sortedSpans) {
    const offsetPct = ((startedAt(span) - traceStart) / totalDuration) * 100;
    const widthPct = Math.max(((span.duration_ms || 0) / totalDuration) * 100, 0.5);
    const depth = depthOf(span);
    const row = document.createElement("div");
    row.className = "waterfall-row";
    row.innerHTML = `
      <div class="waterfall-label" style="padding-left:${depth * 18}px">
        ${span.success === false ? '<span class="error-dot"></span>' : ""}
        <span class="waterfall-kind">${span.item_type === "request" ? "Request" : "Dependency"}</span>
        <span title="${escapeHtml(span.name)}">${escapeHtml(span.name)}</span>
      </div>
      <div class="waterfall-track">
        <div class="waterfall-bar${span.success === false ? " error" : ""}" style="left:${offsetPct}%;width:${widthPct}%"></div>
      </div>
      <div class="waterfall-duration">${formatDurationMs(span.duration_ms)}</div>
    `;
    row.addEventListener("click", () => openDetailPanel(span));
    waterfall.appendChild(row);
  }

  const eventsTable = el("ai-trace-events-table");
  const eventsBody = eventsTable.querySelector("tbody");
  const eventsEmpty = el("ai-trace-events-empty");
  eventsBody.innerHTML = "";
  if (events.length === 0) {
    eventsEmpty.classList.remove("hidden");
    eventsTable.classList.add("hidden");
  } else {
    eventsEmpty.classList.add("hidden");
    eventsTable.classList.remove("hidden");
    for (const ev of [...events].sort((a, b) => startedAt(a) - startedAt(b))) {
      const tr = document.createElement("tr");
      tr.innerHTML = `
        <td class="mono">${new Date(ev.time || ev.received_at).toLocaleString()}</td>
        <td>${ev.item_type === "exception" ? '<span class="pill pill-danger">Exception</span>' : severityPill(ev.severity ?? 1)}</td>
        <td class="body-cell link-cell" data-open-detail>${escapeHtml(ev.name)}</td>
        <td class="col-actions">${iconBtn("kebab", "View details", `data-event-detail="${ev.id}"`)}</td>
      `;
      tr.querySelector("[data-event-detail]").addEventListener("click", () => openDetailPanel(ev));
      tr.querySelector("[data-open-detail]").addEventListener("click", () => openDetailPanel(ev));
      eventsBody.appendChild(tr);
    }
  }
}

async function loadLogs() {
  if (!currentInstanceId) return;
  const level = el("ai-log-level-filter").value;
  const search = el("ai-log-search").value.trim();
  const params = new URLSearchParams();
  if (level !== "") params.set("severity", level);
  if (search) params.set("search", search);

  let logs;
  try {
    logs = await api(`/api/app-insights/${currentInstanceId}/logs?${params.toString()}`);
  } catch (err) {
    toast("error", `Failed to load structured logs: ${err.message}`);
    return;
  }

  const table = el("ai-logs-table");
  const body = table.querySelector("tbody");
  const empty = el("ai-logs-empty");
  body.innerHTML = "";

  if (logs.length === 0) {
    empty.classList.remove("hidden");
    table.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  table.classList.remove("hidden");

  for (const log of logs) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td>${severityPill(log.severity ?? 1)}</td>
      <td class="mono">${new Date(log.time || log.received_at).toLocaleString()}</td>
      <td class="body-cell link-cell" data-open-detail>${escapeHtml(log.name)}</td>
      <td>${traceCell(log)}</td>
      <td class="col-actions">${iconBtn("kebab", "View details", `data-log-detail="${log.id}"`)}</td>
    `;
    tr.querySelector("[data-log-detail]").addEventListener("click", () => openDetailPanel(log));
    tr.querySelector("[data-open-detail]").addEventListener("click", () => openDetailPanel(log));
    wireTraceCells(tr);
    body.appendChild(tr);
  }
}

el("ai-log-level-filter").addEventListener("change", () => {
  if (currentAiView === "logs") loadLogs();
});
let logSearchDebounce;
el("ai-log-search").addEventListener("input", () => {
  clearTimeout(logSearchDebounce);
  logSearchDebounce = setTimeout(() => {
    if (currentAiView === "logs") loadLogs();
  }, 200);
});

/** Metric telemetry's actual name/value live in `data.metrics[0]` (an array with one entry
 * per `TrackMetric` call), not at the envelope's top level - see
 * `emu-appinsights-core`'s `summary_name()`. */
function metricValue(item) {
  const m = item.data && Array.isArray(item.data.metrics) ? item.data.metrics[0] : null;
  return m && typeof m.value === "number" ? m.value : null;
}

async function loadMetrics() {
  if (!currentInstanceId) return;
  let items;
  try {
    items = await api(`/api/app-insights/${currentInstanceId}/items?type=metric`);
  } catch (err) {
    toast("error", `Failed to load metrics: ${err.message}`);
    return;
  }

  const table = el("ai-metrics-table");
  const body = table.querySelector("tbody");
  const empty = el("ai-metrics-empty");
  body.innerHTML = "";

  if (items.length === 0) {
    empty.classList.remove("hidden");
    table.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  table.classList.remove("hidden");

  for (const item of items) {
    const tr = document.createElement("tr");
    const value = metricValue(item);
    tr.innerHTML = `
      <td class="mono">${new Date(item.time || item.received_at).toLocaleString()}</td>
      <td class="body-cell link-cell" data-open-detail>${escapeHtml(item.name)}</td>
      <td class="mono">${value === null ? "—" : value}</td>
      <td class="col-actions">${iconBtn("kebab", "View details", `data-metric-detail="${item.id}"`)}</td>
    `;
    tr.querySelector("[data-metric-detail]").addEventListener("click", () => openDetailPanel(item));
    tr.querySelector("[data-open-detail]").addEventListener("click", () => openDetailPanel(item));
    body.appendChild(tr);
  }
}

el("ai-other-type-filter").addEventListener("change", () => {
  if (currentAiView === "other") loadOther();
});

async function loadOther() {
  if (!currentInstanceId) return;
  const type = el("ai-other-type-filter").value;
  let items;
  try {
    items = await api(`/api/app-insights/${currentInstanceId}/items?type=${type}`);
  } catch (err) {
    toast("error", `Failed to load telemetry: ${err.message}`);
    return;
  }

  const table = el("ai-other-table");
  const body = table.querySelector("tbody");
  const empty = el("ai-other-empty");
  body.innerHTML = "";

  if (items.length === 0) {
    empty.classList.remove("hidden");
    table.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  table.classList.remove("hidden");

  for (const item of items) {
    const tr = document.createElement("tr");
    const result = item.success === false ? "Failed" : item.success === true ? "Success" : "—";
    tr.innerHTML = `
      <td class="mono">${new Date(item.time || item.received_at).toLocaleString()}</td>
      <td class="body-cell link-cell" data-open-detail>${escapeHtml(item.name)}</td>
      <td>${escapeHtml(result)}</td>
      <td class="col-actions">${iconBtn("kebab", "View details", `data-other-detail="${item.id}"`)}</td>
    `;
    tr.querySelector("[data-other-detail]").addEventListener("click", () => openDetailPanel(item));
    tr.querySelector("[data-open-detail]").addEventListener("click", () => openDetailPanel(item));
    body.appendChild(tr);
  }
}

document.querySelectorAll("#ai-subnav .tab-btn").forEach((btn) => {
  btn.addEventListener("click", () => showAiView(btn.dataset.aiView));
});

el("ai-clear-btn").addEventListener("click", async () => {
  if (!currentInstanceId) return;
  if (!(await confirmDialog("Clear all captured telemetry for this instance?", { confirmText: "Clear" }))) return;
  try {
    await api(`/api/app-insights/${currentInstanceId}/items`, { method: "DELETE" });
    toast("success", "Telemetry cleared");
  } catch (err) {
    toast("error", err.message);
  }
  await loadTraces();
});

// ---------------------------------------------------------------- events

document.querySelectorAll('[data-nav="dashboard"]').forEach((a) => a.addEventListener("click", (ev) => {
  ev.preventDefault();
  navDashboard();
}));

document.querySelectorAll('[data-nav="running"]').forEach((a) => a.addEventListener("click", (ev) => {
  ev.preventDefault();
  navRunning();
}));

document.querySelectorAll('[data-nav="all-resources"]').forEach((a) => a.addEventListener("click", (ev) => {
  ev.preventDefault();
  navAllResources();
}));

el("all-resources-new-btn").addEventListener("click", () => openNewResourceModal(null));

el("sb-new-queue-btn").addEventListener("click", () => openModal("modal-new-queue"));
el("sb-new-queue-btn-2").addEventListener("click", () => openModal("modal-new-queue"));

el("sb-queue-search").addEventListener("input", (ev) => {
  queueFilter = ev.target.value;
  loadQueues();
});

document.querySelectorAll("#sb-tabs .tab-btn").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll("#sb-tabs .tab-btn").forEach((b) => b.classList.remove("active"));
    btn.classList.add("active");
    currentState = btn.dataset.state;
    loadMessages();
  });
});

el("sb-purge-btn").addEventListener("click", async () => {
  if (!currentInstanceId || !currentQueue) return;
  if (!(await confirmDialog(`Purge all active messages from "${currentQueue}"?`, { confirmText: "Purge" }))) return;
  try {
    await api(`/api/service-bus/${currentInstanceId}/queues/${currentQueue}/purge`, { method: "POST" });
    toast("success", "Queue purged");
  } catch (err) {
    toast("error", err.message);
  }
  await loadMessages();
});

el("sb-send-btn").addEventListener("click", () => openModal("modal-send-message"));

el("sb-create-queue-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!currentInstanceId) return;
  const input = el("sb-new-queue-name");
  const name = input.value.trim();
  if (!name) return;
  try {
    await api(`/api/service-bus/${currentInstanceId}/queues`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
    toast("success", `Queue "${name}" created`);
    input.value = "";
    closeModal("modal-new-queue");
    await loadQueues();
  } catch (err) {
    toast("error", err.message);
  }
});

el("sb-send-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!currentInstanceId || !currentQueue) return;
  const textarea = el("sb-send-body");
  const sessionInput = el("sb-send-session-id");
  try {
    await api(`/api/service-bus/${currentInstanceId}/queues/${currentQueue}/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ body: textarea.value, session_id: sessionInput.value.trim() || null }),
    });
    toast("success", "Message sent");
    textarea.value = "";
    sessionInput.value = "";
    closeModal("modal-send-message");
    await loadMessages();
  } catch (err) {
    toast("error", err.message);
  }
});

// ------------------------------------------------------------------- boot

(async function boot() {
  await Promise.all([loadResourceKinds(), refreshAll()]);
  renderSidebar();
  applyLocationRoute();
})();

setInterval(() => {
  if (view === "dashboard" || view === "group" || view === "running" || view === "kind" || view === "all-resources") refreshAll();
  else if (view === "servicebus") loadQueues();
  else if (view === "queue") loadMessages();
}, 4000);
