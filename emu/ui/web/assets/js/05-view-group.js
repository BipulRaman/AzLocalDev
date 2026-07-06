// ------------------------------------------------------------- group view

/** Builds one `<tr>` for a resource row - shared by every resource-listing view (group
 * detail, all-resources, running-resources, per-kind), which only differ in which optional
 * columns are shown (controlled via `opts`). Consolidated from four near-identical
 * copy-pasted row-builder functions specifically because that duplication already caused a
 * real bug once (one copy missing the toggle-spinner markup another had) - a single shared
 * builder means a markup/behavior fix here always applies to every view at once. */
function engineRow(eng, opts = {}) {
  const { showType = true, showGroup = false, showActions = true } = opts;
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
    ${showType ? `<td>${typeLabel}</td>` : ""}
    ${showGroup ? `<td class="link-cell" data-open-group-cell="${eng.group_id}">${groupName(eng.group_id)}</td>` : ""}
    <td class="col-actions">${iconBtn("info", "View details", `data-details="${eng.id}"`)}</td>
    ${
      showActions
        ? `<td class="col-actions">
      <div class="row-actions">
        ${iconBtn("edit", "Rename", `data-rename-engine="${eng.id}"`)}
        ${iconBtn("trash", "Delete", `data-delete-engine="${eng.id}"`, "icon-btn-danger")}
      </div>
    </td>`
        : ""
    }
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
      { label: "ManagedIdentityEndpoint", title: "AMQPS endpoint (Managed Identity)" },
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
    body.appendChild(engineRow(eng, { showGroup: true }));
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
    body.appendChild(engineRow(eng, { showGroup: true, showActions: false }));
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
    body.appendChild(engineRow(eng, { showType: false, showGroup: true }));
  }
  wireEngineRowEvents(body);
}

