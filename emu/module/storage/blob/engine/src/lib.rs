//! The Azure Storage (Blob) emulator module: owns an `emu_storage_blob_core::BlobStore` +
//! HTTP listener task speaking the real Blob REST wire protocol (the [`BlobEngine`],
//! implementing the generic `EmulatorEngine` trait from `emu-registry`), plus this module's
//! own axum [`router`] exposing the container/blob data API that the dashboard UI nests
//! under `/api/storage-blob`.
//!
//! Follows the same template as `emu-servicebus-engine`: a self-contained crate providing
//! an `EmulatorEngine` impl + its own API routes, with `emu/services/engine` and
//! `emu/ui/*` staying untouched.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use emu_registry::EmulatorEngine;
use emu_storage_blob_core::{BlobStore, ContainerSummary, CoreError, StoreDump};

/// How often the running store's state is flushed to disk in the background - the same
/// crash-safety net `emu-servicebus-engine` uses, since `BlobEngine::stop()` never runs on
/// an abrupt process exit.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(5);

/// Fixed pseudo account name every instance uses, matching the Azurite convention that
/// Azure Storage SDKs and the Functions host already expect from a local emulator
/// connection string.
const ACCOUNT_NAME: &str = "devstoreaccount1";

/// Offset added to an instance's plain HTTP port to get its HTTPS port. `TokenCredential`
/// (Managed-Identity-style) clients always require TLS - Azure Core's bearer-token auth
/// policy refuses to attach a token to a non-HTTPS request outright - so a second, real TLS
/// listener is needed alongside the plain HTTP one used for account-key connection strings.
/// Derived from the HTTP port (rather than tracked separately) so no changes are needed to
/// the dashboard's port-allocation/collision-avoidance logic.
const HTTPS_PORT_OFFSET: u16 = 10000;

struct RunningState {
    store: BlobStore,
    handle: JoinHandle<()>,
    https_handle: JoinHandle<()>,
    autosave_handle: JoinHandle<()>,
}

/// The Storage (Blob) emulator engine: owns a [`BlobStore`] and the HTTP listener task
/// speaking the Blob REST wire protocol. Each instance is independent - a user can create
/// several (e.g. "Uploads", "Function App Storage"), each with its own id, display name,
/// and port.
pub struct BlobEngine {
    id: String,
    name: StdMutex<String>,
    port: u16,
    state: Mutex<Option<RunningState>>,
}

impl BlobEngine {
    pub fn new(id: impl Into<String>, name: impl Into<String>, port: u16) -> Self {
        Self {
            id: id.into(),
            name: StdMutex::new(name.into()),
            port,
            state: Mutex::new(None),
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// This instance's dedicated HTTPS port (for `TokenCredential`/Managed-Identity-style
    /// clients) - see [`HTTPS_PORT_OFFSET`].
    pub fn https_port(&self) -> u16 {
        self.port + HTTPS_PORT_OFFSET
    }

    /// Dev-only connection string, ready to drop into `AzureWebJobsStorage`-style app
    /// settings or an `Azure.Storage.Blobs` `BlobServiceClient` connection string. The
    /// emulator accepts any credentials, so `AccountKey=emulator` is just a placeholder to
    /// copy/paste as-is - same philosophy as the Service Bus engine's connection string.
    pub fn connection_string(&self) -> String {
        format!(
            "DefaultEndpointsProtocol=http;AccountName={ACCOUNT_NAME};AccountKey=emulator;BlobEndpoint=http://127.0.0.1:{}/{ACCOUNT_NAME};",
            self.port
        )
    }

    /// The blob service endpoint an account-key-based `BlobServiceClient` should point at.
    pub fn blob_service_uri(&self) -> String {
        format!("http://127.0.0.1:{}/{ACCOUNT_NAME}", self.port)
    }

    /// The blob service endpoint a `TokenCredential`-based client (the local stand-in for
    /// Managed Identity - see [`BlobEngine::managed_identity_config`]) should point at, e.g.
    /// `BlobServiceClient(new Uri(...), new DefaultAzureCredential())`, or the
    /// `AzureWebJobsStorage__blobServiceUri` app setting for an identity-based Functions host
    /// storage connection. Unlike [`BlobEngine::blob_service_uri`], this must be `https://` on
    /// the dedicated [`BlobEngine::https_port`] - Azure Core's bearer-token auth policy
    /// refuses to send a token over plain HTTP.
    pub fn https_blob_service_uri(&self) -> String {
        format!("https://127.0.0.1:{}/{ACCOUNT_NAME}", self.https_port())
    }

    /// Config a local app can use to authenticate the way it would with Managed Identity in
    /// Azure, without any code changes beyond constructing the client from a `TokenCredential`
    /// (e.g. `DefaultAzureCredential`) and this URI instead of a connection string - same
    /// approach as `ServiceBusEngine::managed_identity_config`. Since real Managed Identity
    /// (IMDS) only exists inside Azure, local testing falls back to whatever identity
    /// `DefaultAzureCredential` resolves on the developer's machine - this emulator never
    /// validates the bearer token it's handed either way (see `emu-storage-blob-server`'s
    /// permissive philosophy), so any resolvable credential works. Requires trusting the
    /// emulator's self-signed dev certificate once (see `emu_dev_cert::load_or_generate`),
    /// the same one-time step `dotnet dev-certs https --trust` solves for local HTTPS.
    pub fn managed_identity_config(&self) -> serde_json::Value {
        serde_json::json!({
            "blobServiceUri": self.https_blob_service_uri(),
            "credential": "managedidentity",
        })
    }

    /// Returns the live [`BlobStore`] if the engine is currently running, so the dashboard
    /// can query container/blob contents.
    pub async fn store(&self) -> Option<BlobStore> {
        self.state.lock().await.as_ref().map(|s| s.store.clone())
    }

    /// The on-disk file this instance's container/blob data is persisted to, under
    /// `%APPDATA%/AzLocalDev/data/storage-blob/{id}.json` (or the OS equivalent of
    /// `dirs::config_dir()`).
    fn data_file(&self) -> PathBuf {
        data_dir().join(format!("{}.json", sanitize_id(&self.id)))
    }
}

fn data_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("AzLocalDev").join("data").join("storage-blob");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Makes an instance id safe to use as a filename: keeps alphanumerics, `-`, and `_`,
/// replaces everything else with `_`.
fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// On-disk shape of a Storage (Blob) instance's persisted container/blob data: the store
/// dump plus the owning instance's `id` stamped directly in the content, so the data is
/// self-describing and can always be verified/looked up by id instead of trusting the
/// filename alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedInstanceData {
    id: String,
    #[serde(flatten)]
    dump: StoreDump,
}

fn load_dump(path: &StdPath, expected_id: &str) -> Option<StoreDump> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<PersistedInstanceData>(&text) {
        Ok(data) => {
            if data.id != expected_id {
                tracing::warn!(
                    path = %path.display(),
                    stamped_id = %data.id,
                    %expected_id,
                    "persisted Storage (Blob) state's stamped id doesn't match this instance, refusing to load it"
                );
                return None;
            }
            Some(data.dump)
        }
        Err(err) => {
            tracing::warn!(?err, path = %path.display(), "failed to parse persisted Storage (Blob) state, starting empty");
            None
        }
    }
}

async fn save_store_state(store: &BlobStore, path: &StdPath, id: &str) {
    let data = PersistedInstanceData {
        id: id.to_string(),
        dump: store.dump(),
    };
    match serde_json::to_vec_pretty(&data) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(path, bytes) {
                tracing::warn!(?err, path = %path.display(), "failed to persist Storage (Blob) state");
            }
        }
        Err(err) => tracing::warn!(?err, "failed to serialize Storage (Blob) state"),
    }
}

#[async_trait]
impl EmulatorEngine for BlobEngine {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "storage-blob"
    }

    fn display_name(&self) -> String {
        self.name.lock().unwrap().clone()
    }

    fn rename(&self, new_name: &str) {
        *self.name.lock().unwrap() = new_name.to_string();
    }

    async fn start(&self) -> anyhow::Result<()> {
        let mut guard = self.state.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        let data_file = self.data_file();
        let store = match load_dump(&data_file, &self.id) {
            Some(dump) => {
                tracing::info!(path = %data_file.display(), "restored persisted Storage (Blob) state");
                BlobStore::restore(dump)
            }
            None => BlobStore::new(),
        };

        let addr: SocketAddr = format!("127.0.0.1:{}", self.port).parse()?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let router = emu_storage_blob_server::router(store.clone());
        let handle = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, router).await {
                tracing::error!(?err, "Storage (Blob) server task exited with an error");
            }
        });

        // The HTTPS listener is what lets `TokenCredential`-based clients (the local stand-in
        // for Managed Identity - see `managed_identity_config`) connect at all, since Azure
        // Core's bearer-token auth policy refuses to send a token over plain HTTP. Cert
        // generation/loading is best-effort: if it fails for some reason, the plain HTTP
        // listener above still works fine for connection-string-based clients, so we only warn
        // instead of failing startup.
        let https_addr: SocketAddr = format!("127.0.0.1:{}", self.https_port()).parse()?;
        let https_handle = match emu_dev_cert::load_or_generate() {
            Ok(dev_cert) => {
                tracing::info!(path = %dev_cert.cert_path.display(), "using dev TLS certificate for Storage (Blob) HTTPS listener");
                let tls_config = axum_server::tls_rustls::RustlsConfig::from_config(dev_cert.server_config.clone());
                let router_for_tls = emu_storage_blob_server::router(store.clone());
                tokio::spawn(async move {
                    if let Err(err) = axum_server::bind_rustls(https_addr, tls_config)
                        .serve(router_for_tls.into_make_service())
                        .await
                    {
                        tracing::error!(?err, "Storage (Blob) HTTPS server task exited with an error");
                    }
                })
            }
            Err(err) => {
                tracing::warn!(?err, "failed to prepare dev TLS certificate; Storage (Blob) HTTPS listener disabled");
                tokio::spawn(std::future::pending::<()>())
            }
        };

        let store_for_autosave = store.clone();
        let autosave_path = data_file.clone();
        let autosave_id = self.id.clone();
        let autosave_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(AUTOSAVE_INTERVAL);
            ticker.tick().await; // first tick fires immediately; skip it, state is already empty/fresh
            loop {
                ticker.tick().await;
                save_store_state(&store_for_autosave, &autosave_path, &autosave_id).await;
            }
        });

        tracing::info!(port = self.port, https_port = self.https_port(), "Storage (Blob) emulator started");
        *guard = Some(RunningState {
            store,
            handle,
            https_handle,
            autosave_handle,
        });
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        let mut guard = self.state.lock().await;
        if let Some(state) = guard.take() {
            state.autosave_handle.abort();
            save_store_state(&state.store, &self.data_file(), &self.id).await;
            state.handle.abort();
            state.https_handle.abort();
            tracing::info!("Storage (Blob) emulator stopped");
        }
        Ok(())
    }

    async fn is_running(&self) -> bool {
        self.state.lock().await.is_some()
    }

    async fn detail(&self) -> Option<String> {
        if self.is_running().await {
            // Appending the Managed-Identity-style fields to the same `;`-separated string
            // means the dashboard's existing connection-string detail view picks them up for
            // free, right alongside the account key connection string - same trick the
            // Service Bus engine uses.
            Some(format!(
                "{};ManagedIdentityBlobServiceUri={};ManagedIdentityCredential=managedidentity",
                self.connection_string(),
                self.https_blob_service_uri()
            ))
        } else {
            None
        }
    }

    fn config(&self) -> serde_json::Value {
        serde_json::json!({ "port": self.port })
    }
}

// --------------------------------------------------------------- web routes

/// Thread-safe lookup table of `instance id -> BlobEngine`, used by this module's axum
/// routes to resolve which instance a request is for (see [`router`]). Kept separate from
/// the generic [`emu_registry::EngineRegistry`] so route handlers can call
/// `BlobEngine`-specific methods directly without downcasting.
#[derive(Clone, Default)]
pub struct StorageBlobRegistry {
    inner: Arc<StdMutex<HashMap<String, Arc<BlobEngine>>>>,
}

impl StorageBlobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, engine: Arc<BlobEngine>) {
        self.inner.lock().unwrap().insert(engine.id().to_string(), engine);
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    pub fn get(&self, id: &str) -> Option<Arc<BlobEngine>> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    /// Every currently-registered Storage (Blob) instance - used at startup to bump the
    /// dashboard's port counter past whatever was just restored from a saved group, so
    /// newly-created instances afterward can't collide with restored ones.
    pub fn all(&self) -> Vec<Arc<BlobEngine>> {
        self.inner.lock().unwrap().values().cloned().collect()
    }
}

fn require_engine(registry: &StorageBlobRegistry, id: &str) -> Result<Arc<BlobEngine>, (StatusCode, String)> {
    registry
        .get(id)
        .ok_or((StatusCode::NOT_FOUND, format!("unknown Storage (Blob) instance '{id}'")))
}

/// This module's own axum router (container/blob data API), keyed by instance id so the
/// dashboard UI can address any number of Storage (Blob) instances the user has created.
/// The dashboard UI mounts this under a path prefix (e.g. `/api/storage-blob`) - route paths
/// here are relative to that.
pub fn router(registry: StorageBlobRegistry) -> Router {
    Router::new()
        .route("/:id/containers", get(list_containers).post(create_container))
        .route("/:id/containers/:name", axum::routing::delete(delete_container))
        .route("/:id/containers/:name/blobs", get(list_blobs))
        .route(
            "/:id/containers/:name/blobs/*blob",
            get(download_blob).put(upload_blob).delete(delete_blob),
        )
        .with_state(registry)
}

async fn list_containers(
    State(registry): State<StorageBlobRegistry>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ContainerSummary>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.list_containers()))
}

#[derive(Deserialize)]
struct CreateContainerBody {
    name: String,
}

async fn create_container(
    State(registry): State<StorageBlobRegistry>,
    Path(id): Path<String>,
    Json(body): Json<CreateContainerBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.create_container(&body.name) {
        Ok(()) => Ok(StatusCode::CREATED),
        Err(CoreError::ContainerAlreadyExists(name)) => {
            Err((StatusCode::CONFLICT, format!("container '{name}' already exists")))
        }
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

async fn delete_container(
    State(registry): State<StorageBlobRegistry>,
    Path((id, name)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_container(&name) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(CoreError::ContainerNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("container '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

async fn list_blobs(
    State(registry): State<StorageBlobRegistry>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.list_blobs(&name) {
        Ok(blobs) => Ok(Json(blobs).into_response()),
        Err(CoreError::ContainerNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("container '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

async fn download_blob(
    State(registry): State<StorageBlobRegistry>,
    Path((id, container, blob)): Path<(String, String, String)>,
) -> Result<Response, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.get_blob(&container, &blob) {
        Ok(entry) => {
            let mut response = entry.data.into_response();
            if let Ok(value) = HeaderValue::from_str(&entry.content_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            Ok(response)
        }
        Err(CoreError::ContainerNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("container '{name}' not found"))),
        Err(CoreError::BlobNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("blob '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

async fn upload_blob(
    State(registry): State<StorageBlobRegistry>,
    Path((id, container, blob)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    // `put_blob` auto-creates the container if it doesn't exist yet, so no separate
    // existence check/creation is needed here.
    store.put_blob(&container, &blob, body, content_type, HashMap::new());
    Ok(StatusCode::CREATED)
}

async fn delete_blob(
    State(registry): State<StorageBlobRegistry>,
    Path((id, container, blob)): Path<(String, String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_blob(&container, &blob) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(CoreError::ContainerNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("container '{name}' not found"))),
        Err(CoreError::BlobNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("blob '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}
