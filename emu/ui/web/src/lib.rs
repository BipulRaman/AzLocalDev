//! Generic dashboard web server: serves the control API (list/start/stop emulator engines)
//! and the static dashboard UI (embedded HTML/CSS/JS). Knows nothing about any specific
//! Azure resource emulator - each module (e.g. `sbemu-servicebus-engine`) provides its own
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
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use sbemu_engine::{EngineRegistry, EngineSummary, ResourceGroup, ResourceKind, Session};

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
        .route("/api/sessions", get(list_sessions).post(save_session))
        .route("/api/sessions/:file", axum::routing::delete(delete_session))
        .route("/api/sessions/:file/load", post(load_session))
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
    Ok(Json(
        EngineSummary::from_engine(engine.as_ref(), req.group_id).await,
    ))
}

async fn delete_engine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .registry
        .remove(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
    Json(state.registry.create_group(&req.name, None))
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

// ------------------------------------------------------------------ sessions

fn sessions_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("EmuEngine").join("sessions");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Keeps saved session file names filesystem-safe (Windows disallows `<>:"/\|?*`).
fn sanitize_file_stem(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string()
    } else {
        cleaned
    }
}

#[derive(Serialize)]
struct SessionSummary {
    file: String,
    name: String,
    created_at: chrono::DateTime<chrono::Utc>,
    resource_count: usize,
}

impl From<(&str, Session)> for SessionSummary {
    fn from((file, session): (&str, Session)) -> Self {
        Self {
            file: file.to_string(),
            name: session.name,
            created_at: session.created_at,
            resource_count: session.resources.len(),
        }
    }
}

async fn list_sessions() -> Result<Json<Vec<SessionSummary>>, (StatusCode, String)> {
    let dir = sessions_dir();
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_str::<Session>(&text) else {
            continue;
        };
        out.push(SessionSummary::from((stem, session)));
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(Json(out))
}

#[derive(Deserialize)]
struct SaveSessionRequest {
    name: Option<String>,
}

async fn save_session(
    State(state): State<AppState>,
    Json(req): Json<SaveSessionRequest>,
) -> Result<Json<SessionSummary>, (StatusCode, String)> {
    let name = req
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
    let session = state.registry.snapshot(name.clone());
    let file_stem = sanitize_file_stem(&name);
    let path = sessions_dir().join(format!("{file_stem}.json"));
    let json = serde_json::to_string_pretty(&session)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    std::fs::write(&path, json).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(SessionSummary::from((file_stem.as_str(), session))))
}

async fn load_session(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let path = sessions_dir().join(format!("{file}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|_| (StatusCode::NOT_FOUND, "session not found".to_string()))?;
    let session: Session =
        serde_json::from_str(&text).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state
        .registry
        .restore(&session)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

async fn delete_session(Path(file): Path<String>) -> Result<StatusCode, (StatusCode, String)> {
    let path = sessions_dir().join(format!("{file}.json"));
    std::fs::remove_file(&path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
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

