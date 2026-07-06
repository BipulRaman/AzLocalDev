//! The [`StorageEngine`] itself: owns a Blob store + Queue store + Table store, each with
//! its own HTTP listener speaking the real respective REST wire protocol, plus persistence
//! (see [`StorageDump`]). Implements the generic `emu_registry::EmulatorEngine` trait.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use emu_registry::EmulatorEngine;
use emu_storage_blob_core::{BlobStore, StoreDump};
use emu_storage_queue_core::{QueueStore, QueueStoreDump};
use emu_storage_table_core::{TableStore, TableStoreDump};

/// Module slug used for this instance's persisted data directory/filenames - see
/// `emu_persistence::data_dir`/`data_file`.
const PERSISTENCE_MODULE: &str = "storage";

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

    /// The on-disk file this instance's Blob/Queue/Table data is persisted to, under
    /// `%APPDATA%/AzLocalDev/data/storage-blob/{id}.json` (or the OS equivalent of
    /// `dirs::config_dir()`).
    fn data_file(&self) -> PathBuf {
        emu_persistence::data_file(PERSISTENCE_MODULE, &self.id)
    }
}

/// On-disk shape of a Storage instance's persisted Blob/Queue/Table data - the three stores'
/// dumps, combined into one payload for `emu_persistence::load`/`save`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StorageDump {
    #[serde(flatten)]
    blob: StoreDump,
    queue_dump: QueueStoreDump,
    table_dump: TableStoreDump,
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
        let restored = emu_persistence::load::<StorageDump>(&data_file, &self.id, "Storage");
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
            Some(r) => QueueStore::restore(r.queue_dump.clone()),
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
            Some(r) => TableStore::restore(r.table_dump.clone()),
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
        let autosave_handle = emu_persistence::spawn_autosave(AUTOSAVE_INTERVAL, move || {
            let store = store_for_autosave.clone();
            let queue_store = queue_store_for_autosave.clone();
            let table_store = table_store_for_autosave.clone();
            let path = autosave_path.clone();
            let id = autosave_id.clone();
            async move {
                let dump = StorageDump {
                    blob: store.dump(),
                    queue_dump: queue_store.dump(),
                    table_dump: table_store.dump(),
                };
                emu_persistence::save(&path, &id, dump, "Storage");
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
            let dump = StorageDump {
                blob: state.store.dump(),
                queue_dump: state.queue_store.dump(),
                table_dump: state.table_store.dump(),
            };
            emu_persistence::save(&self.data_file(), &self.id, dump, "Storage");
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
