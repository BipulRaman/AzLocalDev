// -------------------------------------------------------------------- icons

// Small inline icon set shared by every icon-only button in the app, so row actions stay
// compact and visually consistent no matter which table they appear in.
const ICONS = {
  info: '<svg viewBox="0 0 20 20" fill="none"><circle cx="10" cy="10" r="7.25" stroke="currentColor" stroke-width="1.5"/><path d="M10 9v4.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/><circle cx="10" cy="6.6" r="0.95" fill="currentColor"/></svg>',
  trash: '<svg viewBox="0 0 20 20" fill="none"><path d="M4 6h12M8 6V4h4v2M6 6l1 10h6l1-10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  copy: '<svg viewBox="0 0 20 20" fill="none"><rect x="7" y="7" width="10" height="10" rx="1.5" stroke="currentColor" stroke-width="1.5"/><path d="M13 7V4.5A1.5 1.5 0 0 0 11.5 3h-7A1.5 1.5 0 0 0 3 4.5v7A1.5 1.5 0 0 0 4.5 13H7" stroke="currentColor" stroke-width="1.5"/></svg>',
  requeue: '<svg viewBox="0 0 20 20" fill="none"><path d="M4 8a6 6 0 0 1 10.4-4.1M16 4v3.5h-3.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/><path d="M16 12a6 6 0 0 1-10.4 4.1M4 16v-3.5h3.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  edit: '<svg viewBox="0 0 20 20" fill="none"><path d="M12.9 3.9 16.1 7.1M4 16l.7-3.2 8.4-8.4a1.4 1.4 0 0 1 2 0l1.5 1.5a1.4 1.4 0 0 1 0 2L8.2 15.3 4 16Z" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  kebab: '<svg viewBox="0 0 20 20" fill="currentColor"><circle cx="10" cy="4.5" r="1.4"/><circle cx="10" cy="10" r="1.4"/><circle cx="10" cy="15.5" r="1.4"/></svg>',
};

/** Renders a compact, icon-only action button (with a tooltip + accessible label) for use
 * inside table rows, where a full text button would waste horizontal space. */
function iconBtn(icon, title, attrs = "", extraClass = "") {
  return `<button type="button" class="icon-btn-sm ${extraClass}" title="${title}" aria-label="${title}" ${attrs}>${ICONS[icon]}</button>`;
}

// -------------------------------------------------------------------- api

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (!res.ok) {
    let message = `${res.status} ${res.statusText}`;
    try {
      const text = await res.text();
      if (text) message = text;
    } catch {
      /* ignore */
    }
    throw new Error(message);
  }
  const text = await res.text();
  return text ? JSON.parse(text) : null;
}

// ------------------------------------------------------------------ toast

function toast(kind, message) {
  const stack = el("toast-stack");
  const node = document.createElement("div");
  node.className = `toast ${kind}`;
  node.textContent = message;
  stack.appendChild(node);
  setTimeout(() => node.remove(), 3500);
}

// ----------------------------------------------------------------- modals

function openModal(id) {
  const modal = el(id);
  modal.classList.remove("hidden");
  const firstInput = modal.querySelector("input, textarea, select");
  if (firstInput) setTimeout(() => firstInput.focus(), 30);
}

function closeModal(id) {
  el(id).classList.add("hidden");
}

/** Opens the shared rename dialog pre-filled with `currentName`, remembering what's being
 * renamed so the submit handler knows which API endpoint to PATCH. */
function openRenameModal(kind, id, currentName) {
  renameTarget = { kind, id };
  el("rename-title").textContent = kind === "group" ? "Rename resource group" : "Rename resource";
  el("rename-name").value = currentName;
  openModal("modal-rename");
}

document.querySelectorAll("[data-close-modal]").forEach((btn) => {
  btn.addEventListener("click", () => btn.closest(".modal-backdrop").classList.add("hidden"));
});
document.querySelectorAll(".modal-backdrop").forEach((backdrop) => {
  backdrop.addEventListener("click", (ev) => {
    if (ev.target === backdrop) backdrop.classList.add("hidden");
  });
});

// --------------------------------------------------------- confirm dialog

// Replaces the native `confirm()` popup with an in-app modal, so destructive actions look
// and feel consistent with the rest of the dashboard.
let confirmResolve = null;

function confirmDialog(message, opts = {}) {
  el("confirm-title").textContent = opts.title || "Are you sure?";
  el("confirm-message").textContent = message;
  el("confirm-ok-btn").textContent = opts.confirmText || "Delete";
  openModal("modal-confirm");
  return new Promise((resolve) => {
    confirmResolve = resolve;
  });
}

function settleConfirm(result) {
  closeModal("modal-confirm");
  if (confirmResolve) {
    confirmResolve(result);
    confirmResolve = null;
  }
}

el("confirm-ok-btn").addEventListener("click", () => settleConfirm(true));
el("confirm-cancel-btn").addEventListener("click", () => settleConfirm(false));
el("modal-confirm").addEventListener("click", (ev) => {
  if (ev.target.id === "modal-confirm") settleConfirm(false);
});

