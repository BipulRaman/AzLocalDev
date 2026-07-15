// ------------------------------------------------------------------ state

let view = "dashboard"; // dashboard | group | servicebus | queue | storage-blob | blob-container | squeue-detail | stable-detail | app-insights | running | kind
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
// Whether the currently-opened Service Bus queue requires a session id on every message.
// `requires_session` is immutable after queue creation, so this can be cached safely.
let currentQueueRequiresSession = false;
let currentContainerName = null;
let currentStorageView = "containers"; // containers | queues | tables
let currentSQueueName = null;
let currentSTableName = null;
let currentAiView = "traces"; // traces | trace-detail | logs | metrics | other
let currentTraceOperationId = null;
let currentKind = null;
let currentKindName = "";
let renameTarget = null; // { kind: "group" | "engine", id: string }

// -------------------------------------------------------------- routing
//
// Every top-level page (and most drill-down pages inside an instance) gets its own real URL
// via the History API, so a hard reload / direct link / browser back-forward lands on the
// same page instead of always resetting to the dashboard. `applyLocationRoute()` parses
// `location.pathname` and re-derives the view state from it (used on first load and on
// `popstate`); `syncUrl()` is called by the nav*/show*View functions to push the matching
// URL whenever the user navigates via clicks. `suppressUrlSync` prevents those same nav
// function calls (made *by* `applyLocationRoute()` itself while restoring state) from
// pushing extra history entries.
let suppressUrlSync = false;

function currentPath() {
  if (view === "running") return "/running";
  if (view === "all-resources") return "/resources";
  if (view === "kind" && currentKind) return `/type/${encodeURIComponent(currentKind)}`;
  if (view === "group" && currentGroupId) return `/group/${encodeURIComponent(currentGroupId)}`;
  if (currentInstanceId && ["servicebus", "queue", "storage-blob", "blob-container", "squeue-detail", "stable-detail", "app-insights"].includes(view)) {
    let path = `/instance/${encodeURIComponent(currentInstanceId)}`;
    if (view === "queue" && currentQueue) path += `/queues/${encodeURIComponent(currentQueue)}`;
    else if (view === "blob-container" && currentContainerName) path += `/containers/${encodeURIComponent(currentContainerName)}`;
    else if (view === "squeue-detail" && currentSQueueName) path += `/queues/${encodeURIComponent(currentSQueueName)}`;
    else if (view === "stable-detail" && currentSTableName) path += `/tables/${encodeURIComponent(currentSTableName)}`;
    else if (view === "storage-blob") path += currentStorageView === "containers" ? "" : `/${currentStorageView}`;
    else if (view === "app-insights") {
      if (currentAiView === "trace-detail" && currentTraceOperationId) path += `/traces/${encodeURIComponent(currentTraceOperationId)}`;
      else if (currentAiView !== "traces") path += `/${currentAiView}`;
    }
    return path;
  }
  return "/";
}

function syncUrl() {
  if (suppressUrlSync) return;
  const path = currentPath();
  if (path !== location.pathname) history.pushState({}, "", path);
}

/** Parses `location.pathname` and restores the app to the matching view - used on first
 * load and whenever the user navigates with the browser's back/forward buttons. Requires
 * `groupCache`/`engineCache`/`kindCache` to already be populated. */
function applyLocationRoute() {
  suppressUrlSync = true;
  try {
    const segments = location.pathname.split("/").filter(Boolean).map((s) => decodeURIComponent(s));
    if (segments.length === 0) {
      navDashboard();
    } else if (segments[0] === "running") {
      navRunning();
    } else if (segments[0] === "resources") {
      navAllResources();
    } else if (segments[0] === "type" && segments[1]) {
      navKind(segments[1]);
    } else if (segments[0] === "group" && segments[1]) {
      navGroup(segments[1]);
    } else if (segments[0] === "instance" && segments[1]) {
      const id = segments[1];
      navInstance(id);
      const engine = engineCache.find((e) => e.id === id);
      const kind = engine ? engine.kind : null;
      const [, , sub, subName] = segments;
      if (kind === "service-bus" && sub === "queues" && subName) {
        navQueue(subName);
      } else if (kind === "storage") {
        if (sub === "containers" && subName) navContainer(subName);
        else if (sub === "queues" && subName) navSQueue(subName);
        else if (sub === "tables" && subName) navSTable(subName);
        else if (sub === "queues" || sub === "tables" || sub === "containers") showStorageView(sub);
      } else if (kind === "app-insights") {
        if (sub === "traces" && subName) openTrace(subName);
        else if (sub === "logs" || sub === "metrics" || sub === "other") showAiView(sub);
      }
    } else {
      navDashboard();
    }
  } finally {
    suppressUrlSync = false;
  }
}

window.addEventListener("popstate", applyLocationRoute);

const el = (id) => document.getElementById(id);

