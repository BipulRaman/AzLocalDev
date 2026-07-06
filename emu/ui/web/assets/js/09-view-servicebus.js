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

