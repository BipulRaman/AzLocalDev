# Session Notes

A chronological log of the major decisions, features, and bug fixes made during development of this
project, kept for future reference (why things are the way they are, and what was deliberately deferred).
For the current architecture itself, see [DESIGN.md](DESIGN.md); for user-facing setup, see the
[README](../README.md).

## Branding & UI polish

- Dashboard rebranded from "enum-engine"/"sbemu" to **"Az.Local.Dev"** (dashboard UI text only at first;
  later the crate/package names themselves were renamed too - see below).
- Tray/dashboard icon went through several redesigns before landing on the final version: a single SVG
  (`emu/ui/web/assets/icon.svg`) - an azure-blue gradient cloud with a centered gray gear, no border/no
  background square - shared identically between the system tray icon (rasterized at runtime via `resvg`)
  and the dashboard favicon/brand mark, so there's exactly one source of truth for the icon.
- Global search (`topbar-search` in the dashboard header) was present in the markup but permanently
  `disabled` and non-functional; implemented properly - live-filters resource groups + resources as you
  type, dropdown results navigate straight to the match, Escape/click-outside closes it.
- Fixed a layout issue where the search box floated immediately next to the logo instead of lining up with
  the content column below it - gave `.topbar-brand` a fixed width (matching the sidebar) so the search box
  starts where the sidebar visually ends.

## Persistence redesign

**Problem**: renamed/created resources didn't survive an app restart. Root cause: `main.rs` always hardcoded
a single fresh "service-bus-1" instance on every startup; the only persistence mechanism was a manual
"save/load session" API that was never exposed in any UI and never triggered automatically.

**Fix**: replaced entirely with always-on auto-persistence, two layers (see
[DESIGN.md §10](DESIGN.md#10-persistence) for the current shape):

- One JSON file per resource group (`%APPDATA%/EmuEngine/groups/{group-id}.json`), rewritten on every
  create/rename/delete, loaded automatically on startup - no manual "save" step exists anymore.
- Per-instance queue/message or container/blob data, flushed on a background timer + clean shutdown.
- Every persisted file (both layers) stamps its own owning id in the content itself, verified against the
  filename-implied id on load - a mismatch is rejected (logged) rather than trusted blindly.
- The old manual `/api/sessions*` routes, `Session`/`SessionResource` types, and `sessions_dir()` were
  removed entirely per explicit instruction ("sessions are no longer needed").

## Crate rename: `sbemu-*` → `emu-*`

Mechanical, workspace-wide rename with no behavior change, prompted by "wht its sbemu ?? i want just emu":

| Old | New |
|---|---|
| `sbemu-core` | `emu-servicebus-core` |
| `sbemu-amqp` | `emu-servicebus-amqp` |
| `sbemu-servicebus-engine` | `emu-servicebus-engine` |
| `sbemu-engine` (generic registry) | `emu-registry` (deliberately *not* `emu-engine`, to avoid colliding with the `emu-engine.exe` binary name) |
| `sbemu-web` | `emu-web` |
| `sbemu-gui` (package; `[[bin]]` stays `emu-engine`) | `emu-gui` |

Verified with a full `cargo build` (exit 0) and a live run afterward.

## Storage (Blob) emulator

Added as a second resource kind (`storage-blob`), following the exact same template Service Bus already
established (see [DESIGN.md §5](DESIGN.md#5-the-emulatorengine-pattern)):

- `emu-storage-blob-core` / `-server` / `-engine`, mirroring `emu-servicebus-*`'s three-crate split.
- Speaks the real Blob REST API (path-style, Azurite's `devstoreaccount1` convention) - containers,
  block-blob upload/download/delete/list, `x-ms-meta-*` metadata. Deliberately out of scope for v1: leases,
  snapshots/versioning, soft delete, large-blob block-list uploads, SAS validation.
- Dashboard UI: new Containers list + Blob list views, upload via a hidden file input PUT-ing straight to
  the dashboard's own API (avoids CORS/needing a second port from the browser's perspective).
- Considered broadening scope to a unified "Storage Account" resource (Blob+Queue+Table together, needed for
  full `AzureWebJobsStorage`/Durable Functions compatibility) but **deliberately scoped down to Blob only**
  per explicit direction; Queue/Table remain a documented future item.

## Managed Identity-style connections (and the HTTPS bug)

Extending the Service Bus's existing "Managed-Identity-style" connection support (an AMQPS listener +
permissive SASL, since real Managed Identity/Entra ID can't be emulated locally at all) to Blob Storage
surfaced a real bug:

- **Symptom**: `AzureWebJobsStorage__blobServiceUri` pointed at the plain `http://127.0.0.1:10000/...`
  endpoint did nothing / failed.
- **Root cause**: Azure Core's bearer-token auth policy refuses outright to attach a `TokenCredential`'s
  token to a non-HTTPS request - this fails client-side before any request even reaches the emulator. (This
  is exactly why Service Bus already needed a dedicated AMQPS listener for this auth style - Blob needed the
  HTTPS equivalent.)
- **Fix**: extracted the previously AMQP-specific dev-cert generation code into a new shared crate,
  `emu-dev-cert`, and added a second, dedicated HTTPS listener to `BlobEngine` (port `<http port> + 10000`,
  via `axum-server`'s rustls integration) using that same shared cert. `managed_identity_config()`/`detail()`
  now correctly report the `https://` endpoint.
- Verified end-to-end: created a resource, confirmed the dashboard's `ManagedIdentityBlobServiceUri` field
  showed the `https://.../:2000x` value, and a direct TLS request to that port returned `200 OK`.
- Also clarified (and documented in the README) that `AzureWebJobsStorage__accountName` can never work
  against a local emulator - it hardcodes resolution to `https://<name>.blob.core.windows.net`, with no way
  to override the scheme or DNS suffix.

## Dev-cert trust: silent attempt → explicit prompt

Originally, `ensure_trusted()` silently ran `certutil -user -addstore Root` once per process and just logged
a warning on failure - easy to miss, and modifies the OS certificate store without asking. Changed to:

- `emu-dev-cert` no longer touches the certificate store on its own. It exposes `DevCertificate::is_trusted()`
  (checks a persisted marker file) and `DevCertificate::trust()` (runs `certutil`, writes the marker on
  success) - callers decide when/whether to call `trust()`.
- `emu-gui` calls this once at startup: if not yet trusted, shows a native Windows Yes/No message box
  explaining what the certificate is for, then runs `trust()` (with a success/failure follow-up message,
  including the manual `certutil` command as a fallback) if the user agrees.

## Windows console window bug

`emu-engine.exe` popped up a visible console window on every launch, despite being a tray-only app - Rust
binaries default to the "console" subsystem on Windows regardless of whether they use a console. Fixed by
adding `#![windows_subsystem = "windows"]` to `main.rs`. Trade-off noted: this also means `tracing`/`RUST_LOG`
output is no longer visible anywhere (a GUI-subsystem process doesn't attach to a console even when launched
from a terminal) - file-based logging would be a future follow-up if needed for debugging.

## Docs reorganization

`DESIGN.md` had drifted far from reality (topics/subscriptions, sled persistence, a separate management REST
API on port 9300, Docker deployment, a Svelte SPA - none of which exist) since it was written before most of
the above happened. Moved to `docs/DESIGN.md` and rewritten from scratch to describe the system as it
actually exists today, plus this notes file, both under a new top-level `docs/` folder.
