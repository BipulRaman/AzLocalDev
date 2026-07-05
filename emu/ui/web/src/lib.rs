//! Generic dashboard web server: serves the control API (list/start/stop emulator engines)
//! and the static dashboard UI (embedded HTML/CSS/JS). Knows nothing about any specific
//! Azure resource emulator - each module (e.g. `emu-servicebus-engine`) provides its own
//! axum router that the composition root (`emu/ui/gui`) nests alongside this one.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::{
    extract::{Path, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use tower_http::cors::CorsLayer;

use emu_registry::{EngineRegistry, EngineSummary, GroupSnapshot, ResourceGroup, ResourceKind};

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

#[derive(Clone)]
pub struct AppState {
    pub registry: EngineRegistry,
}

/// The generic dashboard router: engine control API only. Callers (the composition root)
/// should `.merge()`/`.nest()` each module's own router onto this before adding the static
/// asset fallback with [`with_static_fallback`] and serving it.
pub fn dashboard_router(state: AppState) -> Router {
    Router::new()
        .route("/api/engines", get(list_engines).post(create_engine))
        .route("/api/engines/:id", axum::routing::delete(delete_engine).patch(rename_engine))
        .route("/api/engines/:id/start", post(start_engine))
        .route("/api/engines/:id/stop", post(stop_engine))
        .route("/api/resource-kinds", get(list_resource_kinds))
        .route("/api/resource-groups", get(list_resource_groups).post(create_resource_group))
        .route("/api/resource-groups/:id", axum::routing::delete(delete_resource_group).patch(rename_resource_group))
        .route("/api/resource-groups/:id/start", post(start_resource_group))
        .route("/api/resource-groups/:id/stop", post(stop_resource_group))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Adds the embedded static dashboard assets (HTML/CSS/JS) as the fallback route. Call this
/// last, after nesting any module-specific routers, since axum resolves the fallback only
/// when no other route matches.
pub fn with_static_fallback(router: Router) -> Router {
    router.fallback(static_asset)
}

pub async fn serve(addr: SocketAddr, router: Router) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "dashboard web server listening");
    axum::serve(listener, router).await?;
    Ok(())
}

// ------------------------------------------------------------------- engines

async fn list_engines(State(state): State<AppState>) -> Json<Vec<EngineSummary>> {
    Json(state.registry.summaries().await)
}

async fn start_engine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = state
        .registry
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, "unknown engine".to_string()))?;
    engine
        .start()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

async fn stop_engine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = state
        .registry
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, "unknown engine".to_string()))?;
    engine
        .stop()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct CreateEngineRequest {
    kind: String,
    name: String,
    group_id: String,
}

async fn create_engine(
    State(state): State<AppState>,
    Json(req): Json<CreateEngineRequest>,
) -> Result<Json<EngineSummary>, (StatusCode, String)> {
    let engine = state
        .registry
        .create(&req.kind, &req.name, &req.group_id)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    persist_group(&state.registry, &req.group_id);
    Ok(Json(
        EngineSummary::from_engine(engine.as_ref(), req.group_id).await,
    ))
}

async fn delete_engine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let group_id = state.registry.group_of(&id);
    state
        .registry
        .remove(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(group_id) = group_id {
        persist_group(&state.registry, &group_id);
    }
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct RenameRequest {
    name: String,
}

async fn rename_engine(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RenameRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .registry
        .rename(&id, req.name.trim())
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    if let Some(group_id) = state.registry.group_of(&id) {
        persist_group(&state.registry, &group_id);
    }
    Ok(StatusCode::OK)
}

async fn list_resource_kinds(State(state): State<AppState>) -> Json<Vec<ResourceKind>> {
    Json(state.registry.kinds())
}

// ------------------------------------------------------------ resource groups

async fn list_resource_groups(State(state): State<AppState>) -> Json<Vec<ResourceGroup>> {
    Json(state.registry.list_groups())
}

#[derive(Deserialize)]
struct CreateResourceGroupRequest {
    name: String,
}

async fn create_resource_group(
    State(state): State<AppState>,
    Json(req): Json<CreateResourceGroupRequest>,
) -> Json<ResourceGroup> {
    let group = state.registry.create_group(&req.name, None);
    persist_group(&state.registry, &group.id);
    Json(group)
}

async fn delete_resource_group(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .registry
        .delete_group(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    delete_group_file(&id);
    Ok(StatusCode::OK)
}

async fn rename_resource_group(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RenameRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .registry
        .rename_group(&id, req.name.trim())
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    persist_group(&state.registry, &id);
    Ok(StatusCode::OK)
}

/// Starts every resource inside the group. Groups are independent - any number of them can
/// be running at the same time.
async fn start_resource_group(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    for engine in state.registry.in_group(&id) {
        engine
            .start()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(StatusCode::OK)
}

/// Stops every resource inside the group.
async fn stop_resource_group(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    for engine in state.registry.in_group(&id) {
        engine
            .stop()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(StatusCode::OK)
}

// ------------------------------------------------------------------ persistence

/// Each resource group is persisted as its own `{group-id}.json` file under this folder -
/// sits alongside the existing `data/` (per-instance queue/message data) and `certs/`
/// folders under the same `%APPDATA%/EmuEngine` base. One file per group (rather than one
/// big file for everything) means every edit only rewrites the group actually touched, and
/// a group's file is simply deleted when the group itself is deleted.
fn groups_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("EmuEngine").join("groups");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn group_file_path(group_id: &str) -> PathBuf {
    groups_dir().join(format!("{group_id}.json"))
}

/// Rewrites `group_id`'s persisted file from the registry's current in-memory state.
/// Best-effort: logs a warning and otherwise does nothing on failure or if the group no
/// longer exists - persistence must never take down a request that already succeeded in
/// memory. Called after every mutation that touches a group's resources or its own name -
/// also public so the composition root (`emu/ui/gui`) can persist the default group it
/// creates on a brand new install (first run, before any user edit has happened).
pub fn persist_group(registry: &EngineRegistry, group_id: &str) {
    let Some(snapshot) = registry.snapshot_group(group_id) else {
        return;
    };
    match serde_json::to_string_pretty(&snapshot) {
        Ok(json) => {
            if let Err(err) = std::fs::write(group_file_path(group_id), json) {
                tracing::warn!(?err, %group_id, "failed to persist resource group");
            }
        }
        Err(err) => tracing::warn!(?err, %group_id, "failed to serialize resource group"),
    }
}

/// Deletes `group_id`'s persisted file (e.g. after the group itself was deleted).
/// Best-effort - a missing file is not an error.
fn delete_group_file(group_id: &str) {
    let _ = std::fs::remove_file(group_file_path(group_id));
}

/// Reads every persisted `{group-id}.json` file and restores each into `registry`,
/// recreating every group and instance exactly as last saved (including any renames).
/// Returns `true` if at least one group was loaded, `false` on first-ever run (empty/
/// missing folder) - callers should fall back to creating a fresh default setup in that
/// case. The caller must register every resource `kind` factory the persisted instances
/// might need *before* calling this, since restoring calls each kind's factory closure.
pub async fn load_all_groups(registry: &EngineRegistry) -> bool {
    let dir = groups_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    let mut restored_any = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(?err, path = %path.display(), "failed to read persisted resource group");
                continue;
            }
        };
        let snapshot: GroupSnapshot = match serde_json::from_str(&text) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::warn!(?err, path = %path.display(), "failed to parse persisted resource group");
                continue;
            }
        };
        if let Err(err) = registry.restore_group(&snapshot).await {
            tracing::warn!(?err, path = %path.display(), "failed to restore persisted resource group");
            continue;
        }
        restored_any = true;
    }
    restored_any
}

// ------------------------------------------------------------------- static

pub async fn static_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                content.data.to_vec(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

