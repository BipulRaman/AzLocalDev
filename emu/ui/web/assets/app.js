// ------------------------------------------------------------------ state

let view = "dashboard"; // dashboard | group | servicebus | queue | running | kind
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
let currentKind = null;
let currentKindName = "";
let renameTarget = null; // { kind: "group" | "engine", id: string }

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

// -------------------------------------------------------------- breadcrumb

function renderBreadcrumbs() {
  const bc = el("breadcrumbs");
  const parts = [{ label: "Home", onClick: navDashboard }];
  if (view === "group" || view === "servicebus" || view === "queue") {
    parts.push({ label: currentGroupName || currentGroupId, onClick: () => navGroup(currentGroupId) });
  }
  if (view === "servicebus" || view === "queue") {
    parts.push({ label: currentInstanceName || currentInstanceId, onClick: () => navInstance(currentInstanceId) });
  }
  if (view === "queue") {
    parts.push({ label: currentQueue, onClick: null });
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
  ["dashboard", "group", "servicebus", "queue", "running", "kind", "all-resources"].forEach((v) => {
    el(`view-${v}`).classList.toggle("hidden", v !== name);
  });
  renderBreadcrumbs();
  renderSidebarActiveState();
}

// ------------------------------------------------------------------- nav

function navDashboard() {
  currentGroupId = null;
  currentInstanceId = null;
  showView("dashboard");
  refreshAll();
}

function navRunning() {
  currentGroupId = null;
  currentInstanceId = null;
  showView("running");
  renderRunningResources();
}

function navAllResources() {
  currentGroupId = null;
  currentInstanceId = null;
  showView("all-resources");
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
  renderKindResources();
}

function navGroup(id) {
  const group = groupCache.find((g) => g.id === id);
  currentGroupId = id;
  currentGroupName = group ? group.name : id;
  el("group-title").textContent = currentGroupName;
  showView("group");
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
  el("sb-instance-title").textContent = currentInstanceName;
  showView("servicebus");
  loadQueues();
}

function navQueue(name) {
  currentQueue = name;
  currentState = "active";
  document.querySelectorAll("#sb-tabs .tab-btn").forEach((b) => b.classList.toggle("active", b.dataset.state === "active"));
  showView("queue");
  el("sb-queue-title").textContent = name;
  loadMessages();
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
  input.disabled = true;
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
      input.disabled = true;
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
      input.disabled = true;
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
    const managedField = fields.find((f) => f.label === "ManagedIdentityNamespace");
    const connectionString = fields
      .filter((f) => !f.label.startsWith("ManagedIdentity"))
      .map((f) => `${f.label}=${f.value}`)
      .join(";");
    rows += detailsRow("Connection string", connectionString);
    if (managedField) {
      rows += detailsRow("fullyQualifiedNamespace (Managed Identity)", managedField.value);
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

loadResourceKinds().then(() => renderSidebar());
navDashboard();

setInterval(() => {
  if (view === "dashboard" || view === "group" || view === "running" || view === "kind" || view === "all-resources") refreshAll();
  else if (view === "servicebus") loadQueues();
  else if (view === "queue") loadMessages();
}, 4000);
