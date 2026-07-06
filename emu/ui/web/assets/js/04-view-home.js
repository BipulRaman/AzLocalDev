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
  // Unlike the per-row toggles (recreated from scratch on every render, so they never carry
  // a stale `.loading` class forward), this is a single static element reused across
  // renders - it must be cleared explicitly here once the group's real state is known.
  setSwitchLoading(input, false);
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

