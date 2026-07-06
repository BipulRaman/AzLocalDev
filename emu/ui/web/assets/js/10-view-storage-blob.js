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
    containers = await api(`/api/storage/${currentInstanceId}/containers`);
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
        await api(`/api/storage/${currentInstanceId}/containers/${name}`, { method: "DELETE" });
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
      `/api/storage/${currentInstanceId}/containers/${currentContainerName}/blobs`
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
    const downloadUrl = `/api/storage/${currentInstanceId}/containers/${currentContainerName}/blobs/${encodeURIComponent(b.name)}`;
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
        await api(`/api/storage/${currentInstanceId}/containers/${currentContainerName}/blobs/${encodeURIComponent(name)}`, {
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

