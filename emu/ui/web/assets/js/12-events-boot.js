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
