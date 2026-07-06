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

