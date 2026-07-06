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
    await api(`/api/storage/${currentInstanceId}/queues`, {
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
    queues = await api(`/api/storage/${currentInstanceId}/queues`);
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
        await api(`/api/storage/${currentInstanceId}/queues/${q.name}`, { method: "DELETE" });
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
    messages = await api(`/api/storage/${currentInstanceId}/queues/${currentSQueueName}/messages`);
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
    await api(`/api/storage/${currentInstanceId}/queues/${currentSQueueName}/messages`, {
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
    await api(`/api/storage/${currentInstanceId}/queues/${currentSQueueName}/messages`, { method: "DELETE" });
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
    await api(`/api/storage/${currentInstanceId}/tables`, {
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
    tables = await api(`/api/storage/${currentInstanceId}/tables`);
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
        await api(`/api/storage/${currentInstanceId}/tables/${t.name}`, { method: "DELETE" });
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
    entities = await api(`/api/storage/${currentInstanceId}/tables/${currentSTableName}/entities`);
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
        await api(`/api/storage/${currentInstanceId}/tables/${currentSTableName}/entities/${encodeURIComponent(e.partition_key)}/${encodeURIComponent(e.row_key)}`, {
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
    await api(`/api/storage/${currentInstanceId}/tables/${currentSTableName}/entities`, {
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

