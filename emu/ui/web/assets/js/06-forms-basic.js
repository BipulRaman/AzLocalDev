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
    await api(`/api/storage/${currentInstanceId}/containers`, {
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
      `/api/storage/${currentInstanceId}/containers/${currentContainerName}/blobs/${encodeURIComponent(file.name)}`,
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

