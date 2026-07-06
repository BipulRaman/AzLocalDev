//! In-app "Check for updates" feature, backed by GitHub Releases via the `self_update` crate.
//!
//! No custom manifest file is needed (unlike e.g. a Tauri-style `latest.json`): `self_update`
//! talks to the GitHub Releases API directly and matches the release asset whose file name
//! contains the current target triple (see the release workflow, which uploads
//! `AzLocalDev-<target-triple>.zip` for exactly this reason).
//!
//! Checking and installing are deliberately separate endpoints (mirroring how desktop apps
//! like VS Code/Tauri-based updaters work): the dashboard UI checks first, asks the user to
//! confirm, and only then triggers the actual download + in-place replace + restart.

use std::env;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use self_update::update::ReleaseUpdate;
use self_update::version::bump_is_greater;
use serde::Serialize;

use crate::AppState;

const REPO_OWNER: &str = "BipulRaman";
const REPO_NAME: &str = "AzLocalDev";
const BIN_NAME: &str = "AzLocalDev";

/// The running app's version - always comes from the GitHub Release tag that produced this
/// build (baked in at compile time by `build.rs` via the `AZLOCALDEV_VERSION` env var the
/// release workflow sets), NOT from `Cargo.toml`/`CARGO_PKG_VERSION`. Local dev builds get a
/// `0.0.0-dev+<sha>` placeholder instead (see `build.rs`).
const APP_VERSION: &str = env!("AZLOCALDEV_VERSION");

#[derive(Serialize)]
pub struct VersionResponse {
    version: &'static str,
}

/// Returns the running app's version, for display in the dashboard's footer.
pub async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        version: APP_VERSION,
    })
}

#[derive(Serialize)]
pub struct UpdateCheckResponse {
    /// One of "available", "up_to_date", or "error".
    status: &'static str,
    /// The newer version available, only set when `status == "available"`.
    version: Option<String>,
    error: Option<String>,
}

fn build_updater() -> self_update::errors::Result<Box<dyn ReleaseUpdate>> {
    self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        // We drive our own confirm/progress UI in the dashboard - suppress self_update's own
        // stdin prompt (this is a windows-subsystem GUI app with no console anyway) and its
        // stdout logging.
        .no_confirm(true)
        .show_output(false)
        .current_version(APP_VERSION)
        .build()
}

/// Checks GitHub Releases for a newer version WITHOUT downloading/installing it. Installing
/// restarts the app, so the UI asks the user first (see [`install_update`]).
pub async fn check_for_update() -> Json<UpdateCheckResponse> {
    let outcome = tokio::task::spawn_blocking(|| -> Result<Option<String>, String> {
        let updater = build_updater().map_err(|e| e.to_string())?;
        let release = updater.get_latest_release().map_err(|e| e.to_string())?;
        let is_newer = bump_is_greater(APP_VERSION, &release.version).map_err(|e| e.to_string())?;
        Ok(is_newer.then_some(release.version))
    })
    .await;

    Json(match outcome {
        Ok(Ok(Some(version))) => UpdateCheckResponse {
            status: "available",
            version: Some(version),
            error: None,
        },
        Ok(Ok(None)) => UpdateCheckResponse {
            status: "up_to_date",
            version: None,
            error: None,
        },
        Ok(Err(error)) => UpdateCheckResponse {
            status: "error",
            version: None,
            error: Some(error),
        },
        Err(join_error) => UpdateCheckResponse {
            status: "error",
            version: None,
            error: Some(join_error.to_string()),
        },
    })
}

/// Downloads and installs the latest available release, then relaunches the app. Only ever
/// invoked after the user explicitly confirms in the dashboard UI (installing restarts the
/// process, so it must never happen automatically).
pub async fn install_update(
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, String)> {
    // Stop every running engine first so persisted state is flushed before the process exits
    // - mirrors what the tray icon's "Quit" menu item does.
    state.registry.stop_all().await;

    let status = tokio::task::spawn_blocking(|| -> Result<self_update::Status, String> {
        let updater = build_updater().map_err(|e| e.to_string())?;
        updater.update().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if status.updated() {
        // Relaunch: spawn a fresh copy of the exe (which now points at the just-replaced
        // binary) and exit this process, so the OS releases its lock on the old file and the
        // new one takes over - the closest equivalent to an installer's auto-restart.
        if let Ok(exe) = env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
        // Give the HTTP response a moment to actually reach the browser before the process
        // (and its web server) disappears.
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::process::exit(0);
        });
    }

    Ok(StatusCode::OK)
}
