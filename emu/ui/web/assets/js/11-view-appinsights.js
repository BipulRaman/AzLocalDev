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

