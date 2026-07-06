//! The unified Azure Storage emulator module: owns a Blob store + Queue store + Table
//! store, each with its own HTTP listener speaking the real respective REST wire protocol
//! (the [`StorageEngine`], implementing the generic `EmulatorEngine` trait from
//! `emu-registry`), plus this module's own axum [`router`] exposing the container/blob,
//! queue/message, and table/entity data APIs that the dashboard UI nests under
//! `/api/storage-blob` (name kept for backwards compatibility with existing persisted
//! resource groups - it now covers all three storage services, not just Blob).
//!
//! One instance = one emulated Storage *account*, exactly like a real Azure Storage account
//! or an Azurite instance: three sequential ports starting at the instance's base port
//! (`base` = Blob, `base+1` = Queue, `base+2` = Table), matching Azurite's own
//! `10000`/`10001`/`10002` convention. Blob/Queue/Table contents are ALL persisted to disk
//! across restarts, in one combined JSON file per instance (see [`PersistedInstanceData`]) -
//! the only thing that doesn't survive a restart is an in-flight queue-message lease
//! (`pop_receipt`), see `emu-storage-queue-core::model::MessageDump`'s doc comment.
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
    extract::{Path, Query, State},
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
use emu_storage_queue_core::{CoreError as QueueCoreError, MessageView, QueueStore, QueueStoreDump, QueueSummary};
use emu_storage_table_core::{CoreError as TableCoreError, EntityView, TableStore, TableStoreDump, TableSummary};

/// How often the running Blob store's state is flushed to disk in the background - the same
/// crash-safety net `emu-servicebus-engine` uses, since `StorageEngine::stop()` never runs on
/// an abrupt process exit.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(5);

/// Fixed pseudo account name every instance uses, matching the Azurite convention that
/// Azure Storage SDKs and the Functions host already expect from a local emulator
/// connection string.
const ACCOUNT_NAME: &str = "devstoreaccount1";

/// Offset added to an instance's plain (Blob) HTTP port to get its HTTPS port.
/// `TokenCredential` (Managed-Identity-style) clients always require TLS - Azure Core's
/// bearer-token auth policy refuses to attach a token to a non-HTTPS request outright - so a
/// second, real TLS listener is needed alongside the plain HTTP one used for account-key
/// connection strings. Derived from the HTTP port (rather than tracked separately) so no
/// changes are needed to the dashboard's port-allocation/collision-avoidance logic.
const HTTPS_PORT_OFFSET: u16 = 10000;

/// Offsets added to the instance's base (Blob) port to get its Queue/Table ports - matches
/// Azurite's fixed `10000`/`10001`/`10002` convention (relative rather than absolute, so
/// each instance still gets its own free block of 3 ports regardless of its base port).
const QUEUE_PORT_OFFSET: u16 = 1;
const TABLE_PORT_OFFSET: u16 = 2;

struct RunningState {
    store: BlobStore,
    queue_store: QueueStore,
    table_store: TableStore,
    handle: JoinHandle<()>,
    https_handle: JoinHandle<()>,
    queue_handle: JoinHandle<()>,
    table_handle: JoinHandle<()>,
    autosave_handle: JoinHandle<()>,
}

/// The unified Storage emulator engine: owns a [`BlobStore`]/[`QueueStore`]/[`TableStore`]
/// and the three HTTP listener tasks speaking their respective REST wire protocols. Each
/// instance is independent - a user can create several (e.g. "Uploads", "Function App
/// Storage"), each with its own id, display name, and base port (Blob/Queue/Table ports are
/// derived from it).
pub struct StorageEngine {
    id: String,
    name: StdMutex<String>,
    port: u16,
    state: Mutex<Option<RunningState>>,
}

impl StorageEngine {
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

    /// This instance's Queue Storage port - see [`QUEUE_PORT_OFFSET`].
    pub fn queue_port(&self) -> u16 {
        self.port + QUEUE_PORT_OFFSET
    }

    /// This instance's Table Storage port - see [`TABLE_PORT_OFFSET`].
    pub fn table_port(&self) -> u16 {
        self.port + TABLE_PORT_OFFSET
    }

    /// Dev-only connection string, ready to drop into `AzureWebJobsStorage`-style app
    /// settings or an `Azure.Storage.Blobs`/`Azure.Storage.Queues`/`Azure.Data.Tables` client
    /// connection string. The emulator accepts any credentials, so `AccountKey=emulator` is
    /// just a placeholder to copy/paste as-is - same philosophy as the Service Bus engine's
    /// connection string. `QueueEndpoint`/`TableEndpoint` point at this instance's real
    /// Queue/Table listeners (see [`Self::queue_port`]/[`Self::table_port`]) - unlike a
    /// Blob-only emulator, this is a fully-functional Storage account, not just a
    /// connection-string-parser-satisfying placeholder.
    pub fn connection_string(&self) -> String {
        format!(
            "DefaultEndpointsProtocol=http;AccountName={ACCOUNT_NAME};AccountKey=emulator;BlobEndpoint=http://127.0.0.1:{}/{ACCOUNT_NAME};QueueEndpoint=http://127.0.0.1:{}/{ACCOUNT_NAME};TableEndpoint=http://127.0.0.1:{}/{ACCOUNT_NAME}",
            self.port,
            self.queue_port(),
            self.table_port()
        )
    }

    /// The blob service endpoint an account-key-based `BlobServiceClient` should point at.
    pub fn blob_service_uri(&self) -> String {
        format!("http://127.0.0.1:{}/{ACCOUNT_NAME}", self.port)
    }

    /// The blob service endpoint a `TokenCredential`-based client (the local stand-in for
    /// Managed Identity - see [`StorageEngine::managed_identity_config`]) should point at, e.g.
    /// `BlobServiceClient(new Uri(...), new DefaultAzureCredential())`, or the
    /// `AzureWebJobsStorage__blobServiceUri` app setting for an identity-based Functions host
    /// storage connection. Unlike [`StorageEngine::blob_service_uri`], this must be `https://` on
    /// the dedicated [`StorageEngine::https_port`] - Azure Core's bearer-token auth policy
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

    /// Returns the live [`QueueStore`] if the engine is currently running, so the dashboard
    /// can query queue/message contents.
    pub async fn queue_store(&self) -> Option<QueueStore> {
        self.state.lock().await.as_ref().map(|s| s.queue_store.clone())
    }

    /// Returns the live [`TableStore`] if the engine is currently running, so the dashboard
    /// can query table/entity contents.
    pub async fn table_store(&self) -> Option<TableStore> {
        self.state.lock().await.as_ref().map(|s| s.table_store.clone())
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

/// On-disk shape of a Storage instance's persisted Blob/Queue/Table data: the three stores'
/// dumps plus the owning instance's `id` stamped directly in the content, so the data is
/// self-describing and can always be verified/looked up by id instead of trusting the
/// filename alone. `queue_dump`/`table_dump` default to empty when absent so older,
/// Blob-only persisted files (from before this module persisted Queue/Table too) still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedInstanceData {
    id: String,
    #[serde(flatten)]
    dump: StoreDump,
    #[serde(default)]
    queue_dump: QueueStoreDump,
    #[serde(default)]
    table_dump: TableStoreDump,
}

/// Everything restored from one instance's persisted data file.
struct RestoredData {
    blob: StoreDump,
    queue: QueueStoreDump,
    table: TableStoreDump,
}

fn load_dump(path: &StdPath, expected_id: &str) -> Option<RestoredData> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<PersistedInstanceData>(&text) {
        Ok(data) => {
            if data.id != expected_id {
                tracing::warn!(
                    path = %path.display(),
                    stamped_id = %data.id,
                    %expected_id,
                    "persisted Storage state's stamped id doesn't match this instance, refusing to load it"
                );
                return None;
            }
            Some(RestoredData {
                blob: data.dump,
                queue: data.queue_dump,
                table: data.table_dump,
            })
        }
        Err(err) => {
            tracing::warn!(?err, path = %path.display(), "failed to parse persisted Storage state, starting empty");
            None
        }
    }
}

async fn save_store_state(store: &BlobStore, queue_store: &QueueStore, table_store: &TableStore, path: &StdPath, id: &str) {
    let data = PersistedInstanceData {
        id: id.to_string(),
        dump: store.dump(),
        queue_dump: queue_store.dump(),
        table_dump: table_store.dump(),
    };
    match serde_json::to_vec_pretty(&data) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(path, bytes) {
                tracing::warn!(?err, path = %path.display(), "failed to persist Storage state");
            }
        }
        Err(err) => tracing::warn!(?err, "failed to serialize Storage (Blob) state"),
    }
}

#[async_trait]
impl EmulatorEngine for StorageEngine {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "storage"
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
        let restored = load_dump(&data_file, &self.id);
        if restored.is_some() {
            tracing::info!(path = %data_file.display(), "restored persisted Storage state");
        }
        let store = match restored.as_ref() {
            Some(r) => BlobStore::restore(r.blob.clone()),
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

        let queue_store = match restored.as_ref() {
            Some(r) => QueueStore::restore(r.queue.clone()),
            None => QueueStore::new(),
        };
        let queue_addr: SocketAddr = format!("127.0.0.1:{}", self.queue_port()).parse()?;
        let queue_listener = tokio::net::TcpListener::bind(queue_addr).await?;
        let queue_router = emu_storage_queue_server::router(queue_store.clone());
        let queue_handle = tokio::spawn(async move {
            if let Err(err) = axum::serve(queue_listener, queue_router).await {
                tracing::error!(?err, "Storage (Queue) server task exited with an error");
            }
        });

        let table_store = match restored.as_ref() {
            Some(r) => TableStore::restore(r.table.clone()),
            None => TableStore::new(),
        };
        let table_addr: SocketAddr = format!("127.0.0.1:{}", self.table_port()).parse()?;
        let table_listener = tokio::net::TcpListener::bind(table_addr).await?;
        let table_router = emu_storage_table_server::router(table_store.clone());
        let table_handle = tokio::spawn(async move {
            if let Err(err) = axum::serve(table_listener, table_router).await {
                tracing::error!(?err, "Storage (Table) server task exited with an error");
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
        let queue_store_for_autosave = queue_store.clone();
        let table_store_for_autosave = table_store.clone();
        let autosave_path = data_file.clone();
        let autosave_id = self.id.clone();
        let autosave_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(AUTOSAVE_INTERVAL);
            ticker.tick().await; // first tick fires immediately; skip it, state is already empty/fresh
            loop {
                ticker.tick().await;
                save_store_state(&store_for_autosave, &queue_store_for_autosave, &table_store_for_autosave, &autosave_path, &autosave_id).await;
            }
        });

        tracing::info!(
            port = self.port,
            https_port = self.https_port(),
            queue_port = self.queue_port(),
            table_port = self.table_port(),
            "Storage emulator started"
        );
        *guard = Some(RunningState {
            store,
            queue_store,
            table_store,
            handle,
            https_handle,
            queue_handle,
            table_handle,
            autosave_handle,
        });
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        let mut guard = self.state.lock().await;
        if let Some(state) = guard.take() {
            state.autosave_handle.abort();
            save_store_state(&state.store, &state.queue_store, &state.table_store, &self.data_file(), &self.id).await;
            state.handle.abort();
            state.https_handle.abort();
            state.queue_handle.abort();
            state.table_handle.abort();
            tracing::info!("Storage emulator stopped");
        }
        Ok(())
    }

    async fn is_running(&self) -> bool {
        self.state.lock().await.is_some()
    }

    async fn detail(&self) -> Option<String> {
        if self.is_running().await {
            // Appending the Managed-Identity-style field to the same `;`-separated string
            // means the dashboard's existing connection-string detail view picks it up for
            // free, right alongside the account key connection string - same trick the
            // Service Bus engine uses. The dashboard (`parseConnectionDetails`/`extraFields`
            // in app.js) strips this back out of the displayed "Connection string" field, so
            // what a user copies for real use (e.g. `AzureWebJobsStorage`) is a proper,
            // standards-only Azure Storage connection string with no extra keys tacked on.
            Some(format!(
                "{};ManagedIdentityBlobServiceUri={}",
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

/// Thread-safe lookup table of `instance id -> StorageEngine`, used by this module's axum
/// routes to resolve which instance a request is for (see [`router`]). Kept separate from
/// the generic [`emu_registry::EngineRegistry`] so route handlers can call
/// `StorageEngine`-specific methods directly without downcasting.
#[derive(Clone, Default)]
pub struct StorageRegistry {
    inner: Arc<StdMutex<HashMap<String, Arc<StorageEngine>>>>,
}

impl StorageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, engine: Arc<StorageEngine>) {
        self.inner.lock().unwrap().insert(engine.id().to_string(), engine);
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    pub fn get(&self, id: &str) -> Option<Arc<StorageEngine>> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    /// Every currently-registered Storage (Blob) instance - used at startup to bump the
    /// dashboard's port counter past whatever was just restored from a saved group, so
    /// newly-created instances afterward can't collide with restored ones.
    pub fn all(&self) -> Vec<Arc<StorageEngine>> {
        self.inner.lock().unwrap().values().cloned().collect()
    }
}

fn require_engine(registry: &StorageRegistry, id: &str) -> Result<Arc<StorageEngine>, (StatusCode, String)> {
    registry.get(id).ok_or((StatusCode::NOT_FOUND, format!("unknown Storage instance '{id}'")))
}

/// This module's own axum router (container/blob, queue/message, and table/entity data
/// APIs), keyed by instance id so the dashboard UI can address any number of Storage
/// instances the user has created. The dashboard UI mounts this under a path prefix (e.g.
/// `/api/storage-blob`) - route paths here are relative to that.
pub fn router(registry: StorageRegistry) -> Router {
    Router::new()
        .route("/:id/containers", get(list_containers).post(create_container))
        .route("/:id/containers/:name", axum::routing::delete(delete_container))
        .route("/:id/containers/:name/blobs", get(list_blobs))
        .route(
            "/:id/containers/:name/blobs/*blob",
            get(download_blob).put(upload_blob).delete(delete_blob),
        )
        .route("/:id/queues", get(list_queues).post(create_queue))
        .route("/:id/queues/:name", axum::routing::delete(delete_queue))
        .route(
            "/:id/queues/:name/messages",
            get(peek_queue_messages).post(send_queue_message).delete(clear_queue_messages),
        )
        .route("/:id/tables", get(list_tables).post(create_table))
        .route("/:id/tables/:name", axum::routing::delete(delete_table))
        .route("/:id/tables/:name/entities", get(query_table_entities).post(insert_table_entity))
        .route(
            "/:id/tables/:name/entities/:partition_key/:row_key",
            axum::routing::delete(delete_table_entity),
        )
        .with_state(registry)
}

async fn list_containers(
    State(registry): State<StorageRegistry>,
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
    State(registry): State<StorageRegistry>,
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
    State(registry): State<StorageRegistry>,
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
    State(registry): State<StorageRegistry>,
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
    State(registry): State<StorageRegistry>,
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
    State(registry): State<StorageRegistry>,
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
    State(registry): State<StorageRegistry>,
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

// ----------------------------------------------------------------- queues

async fn list_queues(State(registry): State<StorageRegistry>, Path(id): Path<String>) -> Result<Json<Vec<QueueSummary>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.list_queues()))
}

#[derive(Deserialize)]
struct CreateQueueBody {
    name: String,
}

async fn create_queue(
    State(registry): State<StorageRegistry>,
    Path(id): Path<String>,
    Json(body): Json<CreateQueueBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.create_queue(&body.name) {
        Ok(()) => Ok(StatusCode::CREATED),
        Err(QueueCoreError::QueueAlreadyExists(name)) => Err((StatusCode::CONFLICT, format!("queue '{name}' already exists"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

async fn delete_queue(State(registry): State<StorageRegistry>, Path((id, name)): Path<(String, String)>) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_queue(&name) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(QueueCoreError::QueueNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("queue '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

/// Dashboard "browse messages" - always a peek (never actually dequeues/leases anything),
/// same philosophy as the Azure Portal's own queue browser.
async fn peek_queue_messages(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<Vec<MessageView>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.peek_messages(&name, 32) {
        Ok(messages) => Ok(Json(messages)),
        Err(QueueCoreError::QueueNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("queue '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

#[derive(Deserialize)]
struct SendMessageBody {
    body: String,
}

async fn send_queue_message(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<SendMessageBody>,
) -> Result<Json<MessageView>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.put_message(&name, body.body, 0, None) {
        Ok(message) => Ok(Json(message)),
        Err(QueueCoreError::QueueNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("queue '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

async fn clear_queue_messages(State(registry): State<StorageRegistry>, Path((id, name)): Path<(String, String)>) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.clear_messages(&name) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(QueueCoreError::QueueNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("queue '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

// ----------------------------------------------------------------- tables

async fn list_tables(State(registry): State<StorageRegistry>, Path(id): Path<String>) -> Result<Json<Vec<TableSummary>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.list_tables()))
}

#[derive(Deserialize)]
struct CreateTableBody {
    name: String,
}

async fn create_table(
    State(registry): State<StorageRegistry>,
    Path(id): Path<String>,
    Json(body): Json<CreateTableBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.create_table(&body.name) {
        Ok(()) => Ok(StatusCode::CREATED),
        Err(TableCoreError::TableAlreadyExists(name)) => Err((StatusCode::CONFLICT, format!("table '{name}' already exists"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

async fn delete_table(State(registry): State<StorageRegistry>, Path((id, name)): Path<(String, String)>) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_table(&name) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(TableCoreError::TableNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("table '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

#[derive(Deserialize)]
struct EntityQuery {
    partition_key: Option<String>,
}

async fn query_table_entities(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
    Query(query): Query<EntityQuery>,
) -> Result<Json<Vec<EntityView>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.query_entities(&name, query.partition_key.as_deref()) {
        Ok(entities) => Ok(Json(entities)),
        Err(TableCoreError::TableNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("table '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

#[derive(Deserialize)]
struct InsertEntityBody {
    partition_key: String,
    row_key: String,
    #[serde(default)]
    properties: serde_json::Map<String, serde_json::Value>,
}

async fn insert_table_entity(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<InsertEntityBody>,
) -> Result<Json<EntityView>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.insert_entity(&name, &body.partition_key, &body.row_key, body.properties) {
        Ok(entity) => Ok(Json(entity)),
        Err(TableCoreError::TableNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("table '{name}' not found"))),
        Err(TableCoreError::EntityAlreadyExists) => Err((StatusCode::CONFLICT, "entity already exists".to_string())),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

async fn delete_table_entity(
    State(registry): State<StorageRegistry>,
    Path((id, name, partition_key, row_key)): Path<(String, String, String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_entity(&name, &partition_key, &row_key, None) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(TableCoreError::TableNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("table '{name}' not found"))),
        Err(TableCoreError::EntityNotFound) => Err((StatusCode::NOT_FOUND, "entity not found".to_string())),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

