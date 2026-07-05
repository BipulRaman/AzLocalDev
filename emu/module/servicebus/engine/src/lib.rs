//! The Azure Service Bus emulator module: owns the [`emu_servicebus_core::Broker`] + AMQP 1.0
//! listener task (the [`ServiceBusEngine`], implementing the generic `EmulatorEngine`
//! trait from `emu-registry`), plus this module's own axum [`router`] exposing the
//! queue/message data API that the dashboard UI nests under `/api/service-bus`.
//!
//! This is the template every future Azure resource module (e.g. Storage Queues) should
//! follow: a self-contained crate providing an `EmulatorEngine` impl + its own API routes,
//! with the generic `emu/services/engine` and `emu/ui/*` crates staying untouched.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use emu_servicebus_core::{Broker, BrokerDump, EntityOptions, EntityStats, MessageState, NewMessage};
use emu_registry::EmulatorEngine;

/// How often the running broker's state is flushed to disk in the background. This is the
/// safety net that protects queue/topic/message data against the process exiting abruptly
/// (crash, task-manager kill, power loss) instead of a clean shutdown, since in those cases
/// `ServiceBusEngine::stop()` never runs.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(5);

/// Port the AMQPS (AMQP-over-TLS) listener always binds to - the same port real Azure Service
/// Bus uses. Every instance listens on this exact port, just on a different loopback address
/// (see `ServiceBusEngine::new`), since Azure SDK clients built from a `TokenCredential` (the
/// local stand-in for Managed Identity) always dial the default AMQPS port for whatever
/// `fullyQualifiedNamespace` they're given, with no way to specify a custom port without a
/// code change.
const AMQPS_PORT: u16 = 5671;

struct RunningState {
    broker: Broker,
    handle: JoinHandle<()>,
    amqps_handle: JoinHandle<()>,
    autosave_handle: JoinHandle<()>,
}

/// The Azure Service Bus emulator engine: owns a [`Broker`] and the AMQP 1.0 listener task.
/// Each instance is independent - a user can create several (e.g. "Orders Bus", "Events Bus"),
/// each with its own id, display name, and AMQP port.
pub struct ServiceBusEngine {
    id: String,
    name: StdMutex<String>,
    amqp_port: u16,
    /// The loopback address (`127.0.0.{n}`) this instance's AMQPS listener binds to, on the
    /// fixed `AMQPS_PORT`. Every `127.0.0.x` address is loopback with no hosts file or DNS
    /// setup needed, so each instance gets its own address that a bare
    /// `fullyQualifiedNamespace` (no port, as real Azure namespaces never have one) can point
    /// at unambiguously - unlike the plain AMQP listener, which distinguishes instances by port
    /// instead, since a connection string can embed a port directly.
    amqps_host: Ipv4Addr,
    state: Mutex<Option<RunningState>>,
}

impl ServiceBusEngine {
    /// `instance_seq` must be unique per running instance (starting at 1) - it becomes the last
    /// octet of this instance's dedicated `127.0.0.{instance_seq}` AMQPS loopback address.
    pub fn new(id: impl Into<String>, name: impl Into<String>, amqp_port: u16, instance_seq: u8) -> Self {
        Self {
            id: id.into(),
            name: StdMutex::new(name.into()),
            amqp_port,
            amqps_host: Ipv4Addr::new(127, 0, 0, instance_seq.max(1)),
            state: Mutex::new(None),
        }
    }

    pub fn amqp_port(&self) -> u16 {
        self.amqp_port
    }

    pub fn amqps_port(&self) -> u16 {
        AMQPS_PORT
    }

    /// The host part of this instance's AMQPS endpoint (a dedicated loopback address, e.g.
    /// `127.0.0.2`) - what a Managed-Identity-style `fullyQualifiedNamespace` should be set to
    /// in order to reach *this specific* instance rather than another one.
    pub fn amqps_host(&self) -> Ipv4Addr {
        self.amqps_host
    }

    /// Dev-only connection string clients can use to connect to this emulator. The emulator
    /// accepts any credentials (see the AMQP adapter's permissive SASL acceptor), so this
    /// fixed key name/value pair is just a convenient placeholder to copy/paste as-is.
    ///
    /// `UseDevelopmentEmulator=true` is the same flag the official Azure Service Bus emulator
    /// uses: it tells the client SDKs (Azure.Messaging.ServiceBus, the Functions Service Bus
    /// extension, etc.) to skip the TLS handshake and connect with plain AMQP directly to the
    /// port in the connection string. Without it, the SDK defaults to wrapping the connection
    /// in TLS, which this emulator's AMQP listener doesn't speak - the client fails with a
    /// connection error (e.g. "ConnectionRefused") instead of ever completing the AMQP
    /// handshake.
    pub fn connection_string(&self) -> String {
        format!(
            "Endpoint=sb://localhost:{};SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=emulator;UseDevelopmentEmulator=true",
            self.amqp_port
        )
    }

    /// Config a Function/app can use to authenticate the way it would with Managed Identity in
    /// Azure, without any code changes - just point `<Connection>__fullyQualifiedNamespace` at
    /// this instead of a connection string (no `<Connection>__credential` override is read by
    /// most apps' own DI wiring, which typically just always constructs a `TokenCredential` -
    /// e.g. `DefaultAzureCredential` - whenever a namespace is configured). Since real Managed
    /// Identity (IMDS) only exists inside Azure, local testing falls back to whatever identity
    /// `DefaultAzureCredential` resolves on the developer's machine (e.g. `az login`, Visual
    /// Studio, VS Code) - this emulator doesn't validate the token either way, so any resolvable
    /// credential works.
    ///
    /// The returned `fullyQualifiedNamespace` is this instance's dedicated loopback address
    /// (e.g. `127.0.0.2`), not a shared "localhost" - that's what lets multiple running
    /// instances be addressed unambiguously, since a bare namespace has no room for a custom
    /// port (real Azure SDK clients always dial the default AMQPS port, 5671, for whatever
    /// namespace they're given).
    ///
    /// Requires trusting the emulator's self-signed dev certificate once (see
    /// [`emu_servicebus_amqp::load_or_generate_dev_cert`]), the same one-time step `dotnet dev-certs
    /// https --trust` solves for local HTTPS.
    pub fn managed_identity_config(&self) -> serde_json::Value {
        serde_json::json!({
            "fullyQualifiedNamespace": self.amqps_host.to_string(),
            "port": AMQPS_PORT,
            "credential": "managedidentity",
        })
    }

    /// Returns the live [`Broker`] if the engine is currently running, so the web UI can
    /// query queue/topic contents.
    pub async fn broker(&self) -> Option<Broker> {
        self.state.lock().await.as_ref().map(|s| s.broker.clone())
    }

    /// The on-disk file this instance's queue/topic/message data is persisted to, under
    /// `%APPDATA%/EmuEngine/data/service-bus/{id}.json` (or the OS equivalent of
    /// `dirs::config_dir()`). Kept separate per instance id so multiple Service Bus
    /// emulators don't clobber each other's data.
    fn data_file(&self) -> PathBuf {
        data_dir().join(format!("{}.json", sanitize_id(&self.id)))
    }
}

/// Directory persisted Service Bus data files live in, created on demand.
fn data_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("EmuEngine").join("data").join("service-bus");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Makes an instance id safe to use as a filename: keeps alphanumerics, `-`, and `_`,
/// replaces everything else with `_`.
fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Loads a previously-persisted [`BrokerDump`] for this instance from disk, if present.
/// The persisted file stamps its owning instance's `id` in the content itself (not just
/// implied by the filename) - if it doesn't match `expected_id`, the data is rejected
/// (logged, not silently loaded) rather than trusting the filename alone, e.g. in case a
/// file was ever copied/renamed by hand.
fn load_dump(path: &StdPath, expected_id: &str) -> Option<BrokerDump> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<PersistedInstanceData>(&text) {
        Ok(data) => {
            if data.id != expected_id {
                tracing::warn!(
                    path = %path.display(),
                    stamped_id = %data.id,
                    %expected_id,
                    "persisted Service Bus state's stamped id doesn't match this instance, refusing to load it"
                );
                return None;
            }
            Some(data.dump)
        }
        Err(err) => {
            tracing::warn!(?err, path = %path.display(), "failed to parse persisted Service Bus state, starting empty");
            None
        }
    }
}

/// On-disk shape of a Service Bus instance's persisted queue/message data: the broker dump
/// plus the owning instance's `id` stamped directly in the content, so the data is
/// self-describing and can always be verified/looked up by id instead of trusting the
/// filename alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedInstanceData {
    id: String,
    #[serde(flatten)]
    dump: BrokerDump,
}

/// Exports the broker's current state and writes it to disk (with `id` stamped in the
/// content - see [`PersistedInstanceData`]), logging (but not failing) on any error -
/// persistence is best-effort and must never take down the emulator.
async fn save_broker_state(broker: &Broker, path: &StdPath, id: &str) {
    let data = PersistedInstanceData {
        id: id.to_string(),
        dump: broker.export().await,
    };
    match serde_json::to_vec_pretty(&data) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(path, bytes) {
                tracing::warn!(?err, path = %path.display(), "failed to persist Service Bus state");
            }
        }
        Err(err) => tracing::warn!(?err, "failed to serialize Service Bus state"),
    }
}

#[async_trait]
impl EmulatorEngine for ServiceBusEngine {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "service-bus"
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

        let broker = Broker::new();

        let data_file = self.data_file();
        if let Some(dump) = load_dump(&data_file, &self.id) {
            broker.import(dump).await;
            tracing::info!(path = %data_file.display(), "restored persisted Service Bus state");
        }

        let addr: SocketAddr = format!("127.0.0.1:{}", self.amqp_port).parse()?;
        let broker_for_task = broker.clone();
        let handle = tokio::spawn(async move {
            if let Err(err) = emu_servicebus_amqp::run_amqp_server(broker_for_task, addr).await {
                tracing::error!(?err, "AMQP server task exited with an error");
            }
        });

        // The AMQPS (TLS) listener is what lets Azure SDK clients built from a `TokenCredential`
        // (the local stand-in for Managed Identity - see `managed_identity_config`) connect at
        // all, since that construction path always requires TLS. Cert generation/loading is
        // best-effort: if it fails for some reason, the plain AMQP listener above still works
        // fine for connection-string-based clients, so we only warn instead of failing startup.
        let amqps_addr: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(self.amqps_host), AMQPS_PORT);
        let amqps_handle = match emu_servicebus_amqp::load_or_generate_dev_cert() {
            Ok(dev_cert) => {
                let broker_for_tls_task = broker.clone();
                tracing::info!(path = %dev_cert.cert_path.display(), "using dev TLS certificate for AMQPS listener");
                tokio::spawn(async move {
                    if let Err(err) =
                        emu_servicebus_amqp::run_amqps_server(broker_for_tls_task, amqps_addr, dev_cert.tls_acceptor).await
                    {
                        tracing::error!(?err, "AMQPS server task exited with an error");
                    }
                })
            }
            Err(err) => {
                tracing::warn!(?err, "failed to prepare dev TLS certificate; AMQPS listener disabled");
                tokio::spawn(std::future::pending::<()>())
            }
        };

        // Periodically flush state to disk so nothing is lost if the process exits
        // abruptly instead of going through the clean `stop()` path below.
        let broker_for_autosave = broker.clone();
        let autosave_path = data_file.clone();
        let autosave_id = self.id.clone();
        let autosave_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(AUTOSAVE_INTERVAL);
            ticker.tick().await; // first tick fires immediately; skip it, state is already empty/fresh
            loop {
                ticker.tick().await;
                save_broker_state(&broker_for_autosave, &autosave_path, &autosave_id).await;
            }
        });

        tracing::info!(port = self.amqp_port, amqps_host = %self.amqps_host, amqps_port = AMQPS_PORT, "Service Bus emulator started");
        *guard = Some(RunningState {
            broker,
            handle,
            amqps_handle,
            autosave_handle,
        });
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        let mut guard = self.state.lock().await;
        if let Some(state) = guard.take() {
            state.autosave_handle.abort();
            save_broker_state(&state.broker, &self.data_file(), &self.id).await;
            state.handle.abort();
            state.amqps_handle.abort();
            tracing::info!("Service Bus emulator stopped");
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
            // free, right alongside the SAS connection string.
            Some(format!(
                "{};ManagedIdentityEndpoint=amqps://{}:{};ManagedIdentityNamespace={};ManagedIdentityCredential=managedidentity",
                self.connection_string(),
                self.amqps_host,
                AMQPS_PORT,
                self.amqps_host
            ))
        } else {
            None
        }
    }

    fn config(&self) -> serde_json::Value {
        serde_json::json!({ "port": self.amqp_port, "seq": self.amqps_host.octets()[3] })
    }
}

// --------------------------------------------------------------- web routes

/// Thread-safe lookup table of `instance id -> ServiceBusEngine`, used by this module's
/// axum routes to resolve which instance a request is for (see [`router`]). Kept separate
/// from the generic [`emu_registry::EngineRegistry`] (which only knows the `EmulatorEngine`
/// trait object) so route handlers can call `ServiceBusEngine`-specific methods directly
/// without downcasting.
#[derive(Clone, Default)]
pub struct ServiceBusRegistry {
    inner: Arc<StdMutex<HashMap<String, Arc<ServiceBusEngine>>>>,
}

impl ServiceBusRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, engine: Arc<ServiceBusEngine>) {
        self.inner
            .lock()
            .unwrap()
            .insert(engine.id().to_string(), engine);
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    pub fn get(&self, id: &str) -> Option<Arc<ServiceBusEngine>> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    /// Every currently-registered Service Bus instance - used at startup to bump the
    /// dashboard's port/instance-seq counters past whatever was just restored from a saved
    /// session, so newly-created instances afterward can't collide with restored ones.
    pub fn all(&self) -> Vec<Arc<ServiceBusEngine>> {
        self.inner.lock().unwrap().values().cloned().collect()
    }
}

fn require_engine(
    registry: &ServiceBusRegistry,
    id: &str,
) -> Result<Arc<ServiceBusEngine>, (StatusCode, String)> {
    registry
        .get(id)
        .ok_or((StatusCode::NOT_FOUND, format!("unknown Service Bus instance '{id}'")))
}

/// This module's own axum router (queues/messages data API), keyed by instance id so the
/// dashboard UI can address any number of Service Bus instances the user has created.
/// The dashboard UI mounts this under a path prefix (e.g. `/api/service-bus`) - route paths
/// here are relative to that.
pub fn router(registry: ServiceBusRegistry) -> Router {
    Router::new()
        .route("/:id/queues", get(list_queues).post(create_queue))
        .route("/:id/queues/:name", delete(delete_queue))
        .route(
            "/:id/queues/:name/messages",
            get(peek_messages).post(send_message),
        )
        .route("/:id/queues/:name/messages/:seq", delete(delete_message))
        .route("/:id/queues/:name/messages/:seq/resubmit", post(resubmit_message))
        .route("/:id/queues/:name/purge", post(purge_queue))
        .with_state(registry)
}

#[derive(Serialize)]
struct QueueSummary {
    name: String,
    stats: EntityStats,
}

async fn list_queues(
    State(registry): State<ServiceBusRegistry>,
    Path(id): Path<String>,
) -> Result<Json<Vec<QueueSummary>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let broker = require_broker(&engine).await?;
    let mut out = Vec::new();
    for name in broker.list_queues() {
        if let Some(handle) = broker.get_queue(&name) {
            let stats = handle
                .stats()
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
            out.push(QueueSummary { name, stats });
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct CreateQueueRequest {
    name: String,
}

async fn create_queue(
    State(registry): State<ServiceBusRegistry>,
    Path(id): Path<String>,
    Json(req): Json<CreateQueueRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let broker = require_broker(&engine).await?;
    broker.create_queue(&req.name, EntityOptions::default());
    Ok(StatusCode::CREATED)
}

async fn delete_queue(
    State(registry): State<ServiceBusRegistry>,
    Path((id, name)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let broker = require_broker(&engine).await?;
    broker.delete_queue(&name);
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct PeekQuery {
    #[serde(default = "default_state")]
    state: String,
    #[serde(default = "default_from")]
    from: i64,
    #[serde(default = "default_count")]
    count: u32,
}

fn default_state() -> String {
    "active".to_string()
}
fn default_from() -> i64 {
    1
}
fn default_count() -> u32 {
    50
}

#[derive(Serialize)]
struct MessageRow {
    sequence_number: i64,
    message_id: String,
    enqueued_time: chrono::DateTime<chrono::Utc>,
    delivery_count: u32,
    body_text: String,
    session_id: Option<String>,
}

fn parse_state(s: &str) -> MessageState {
    match s {
        "scheduled" => MessageState::Scheduled,
        "deferred" => MessageState::Deferred,
        "deadlettered" | "dead-letter" | "deadletter" => MessageState::DeadLettered,
        _ => MessageState::Active,
    }
}

async fn peek_messages(
    State(registry): State<ServiceBusRegistry>,
    Path((id, name)): Path<(String, String)>,
    Query(q): Query<PeekQuery>,
) -> Result<Json<Vec<MessageRow>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let broker = require_broker(&engine).await?;
    let handle = broker
        .get_queue(&name)
        .ok_or((StatusCode::NOT_FOUND, "unknown queue".to_string()))?;
    let messages = handle
        .peek(parse_state(&q.state), q.from, q.count)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    let rows = messages
        .into_iter()
        .map(|m| MessageRow {
            sequence_number: m.sequence_number,
            message_id: m.message_id,
            enqueued_time: m.enqueued_time,
            delivery_count: m.delivery_count,
            body_text: String::from_utf8_lossy(&m.body).to_string(),
            session_id: m.session_id,
        })
        .collect();
    Ok(Json(rows))
}

#[derive(Deserialize)]
struct SendMessageRequest {
    body: String,
    content_type: Option<String>,
    #[serde(default)]
    properties: HashMap<String, String>,
    session_id: Option<String>,
}

async fn send_message(
    State(registry): State<ServiceBusRegistry>,
    Path((id, name)): Path<(String, String)>,
    Json(req): Json<SendMessageRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let broker = require_broker(&engine).await?;
    let handle = broker
        .get_queue(&name)
        .ok_or((StatusCode::NOT_FOUND, "unknown queue".to_string()))?;
    let msg = NewMessage {
        body: req.body.into_bytes(),
        content_type: req.content_type,
        properties: req.properties,
        session_id: req.session_id.filter(|s| !s.trim().is_empty()),
        ..Default::default()
    };
    handle
        .send_message(msg)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    Ok(StatusCode::CREATED)
}

async fn purge_queue(
    State(registry): State<ServiceBusRegistry>,
    Path((id, name)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let broker = require_broker(&engine).await?;
    let handle = broker
        .get_queue(&name)
        .ok_or((StatusCode::NOT_FOUND, "unknown queue".to_string()))?;
    handle
        .purge()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    Ok(StatusCode::OK)
}

async fn delete_message(
    State(registry): State<ServiceBusRegistry>,
    Path((id, name, seq)): Path<(String, String, i64)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let broker = require_broker(&engine).await?;
    let handle = broker
        .get_queue(&name)
        .ok_or((StatusCode::NOT_FOUND, "unknown queue".to_string()))?;
    let found = handle
        .delete_message(seq)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    if found {
        Ok(StatusCode::OK)
    } else {
        Err((StatusCode::NOT_FOUND, "message not found".to_string()))
    }
}

#[derive(Serialize)]
struct ResubmitResponse {
    sequence_number: i64,
}

/// Moves a dead-lettered message (`seq`) back into the active queue as a brand new message
/// (fresh sequence number, delivery count reset, dead-letter reason/description cleared) -
/// the "move as fresh message" action exposed from the Dead Letter tab in the dashboard UI.
async fn resubmit_message(
    State(registry): State<ServiceBusRegistry>,
    Path((id, name, seq)): Path<(String, String, i64)>,
) -> Result<Json<ResubmitResponse>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let broker = require_broker(&engine).await?;
    let handle = broker
        .get_queue(&name)
        .ok_or((StatusCode::NOT_FOUND, "unknown queue".to_string()))?;
    let new_seq = handle.resubmit_dead_letter(seq).await.map_err(|e| {
        let status = match e {
            emu_servicebus_core::CoreError::SequenceNotFound => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, format!("{e}"))
    })?;
    Ok(Json(ResubmitResponse { sequence_number: new_seq }))
}

async fn require_broker(engine: &ServiceBusEngine) -> Result<Broker, (StatusCode, String)> {
    engine.broker().await.ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Service Bus emulator is not running".to_string(),
    ))
}

