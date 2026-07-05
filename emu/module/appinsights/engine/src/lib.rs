//! The Application Insights emulator module: owns an `emu_appinsights_core::TelemetryStore`
//! and an HTTP listener task speaking the real Breeze ingestion protocol (`POST /v2/track`)
//! that the Application Insights SDKs use (the [`AppInsightsEngine`], implementing the
//! generic `EmulatorEngine` trait from `emu-registry`), plus this module's own axum
//! [`router`] exposing the captured-telemetry query API that the dashboard UI nests under
//! `/api/app-insights`.
//!
//! Follows the same template as `emu-storage-blob-engine`: a self-contained crate providing
//! an `EmulatorEngine` impl + its own API routes, with `emu/services/engine` and
//! `emu/ui/*` staying untouched. Unlike Storage (Blob)/Service Bus, captured telemetry is
//! intentionally *not* persisted to disk (see `emu-appinsights-core`'s `MAX_ITEMS` doc
//! comment) - it's transient debugging data, not a resource an app depends on existing
//! across restarts - so this engine only needs to persist its port + instrumentation key
//! (via [`AppInsightsEngine::config`]) to recreate the same connection string on restore.

use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tower_http::cors::CorsLayer;

use emu_appinsights_core::{TelemetryItem, TelemetryStore, TelemetryType, TraceSummary};
use emu_registry::EmulatorEngine;

struct RunningState {
    store: TelemetryStore,
    handle: JoinHandle<()>,
}

/// The Application Insights emulator engine: owns a [`TelemetryStore`] and the HTTP
/// listener task speaking the Breeze ingestion wire protocol. Each instance is independent,
/// so a user can create several (e.g. "Web App", "Function App"), each with its own id,
/// display name, port, and instrumentation key.
pub struct AppInsightsEngine {
    id: String,
    name: StdMutex<String>,
    port: u16,
    instrumentation_key: String,
    state: Mutex<Option<RunningState>>,
}

impl AppInsightsEngine {
    pub fn new(id: impl Into<String>, name: impl Into<String>, port: u16, instrumentation_key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: StdMutex::new(name.into()),
            port,
            instrumentation_key: instrumentation_key.into(),
            state: Mutex::new(None),
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn instrumentation_key(&self) -> &str {
        &self.instrumentation_key
    }

    /// Dev-only connection string, ready to drop into `APPLICATIONINSIGHTS_CONNECTION_STRING`
    /// or an `ApplicationInsightsServiceOptions.ConnectionString`. The SDKs POST telemetry to
    /// `IngestionEndpoint` over plain HTTP with no auth beyond the instrumentation key
    /// embedded in each envelope, so - unlike Service Bus/Storage Blob - no TLS listener or
    /// Managed-Identity equivalent is needed here at all.
    pub fn connection_string(&self) -> String {
        format!(
            "InstrumentationKey={};IngestionEndpoint=http://127.0.0.1:{}/;LiveEndpoint=http://127.0.0.1:{}/;ApplicationId={}",
            self.instrumentation_key, self.port, self.port, self.instrumentation_key
        )
    }

    /// Returns the live [`TelemetryStore`] if the engine is currently running, so the
    /// dashboard can query captured telemetry.
    pub async fn store(&self) -> Option<TelemetryStore> {
        self.state.lock().await.as_ref().map(|s| s.store.clone())
    }
}

#[async_trait]
impl EmulatorEngine for AppInsightsEngine {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "app-insights"
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

        let store = TelemetryStore::new();
        let addr: SocketAddr = format!("127.0.0.1:{}", self.port).parse()?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let router = ingestion_router(store.clone());
        let handle = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, router).await {
                tracing::error!(?err, "Application Insights ingestion server task exited with an error");
            }
        });

        tracing::info!(port = self.port, "Application Insights emulator started");
        *guard = Some(RunningState { store, handle });
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        let mut guard = self.state.lock().await;
        if let Some(state) = guard.take() {
            state.handle.abort();
            tracing::info!("Application Insights emulator stopped");
        }
        Ok(())
    }

    async fn is_running(&self) -> bool {
        self.state.lock().await.is_some()
    }

    async fn detail(&self) -> Option<String> {
        if self.is_running().await {
            // Appending the OTLP endpoint to the same `;`-separated string means the
            // dashboard's existing connection-string details view picks it up for free,
            // right alongside the Breeze connection string - same trick the Service Bus/
            // Storage Blob engines use for their own Managed-Identity-style extra fields.
            Some(format!(
                "{};OtlpEndpoint=http://127.0.0.1:{};OtlpProtocol=http/json",
                self.connection_string(),
                self.port
            ))
        } else {
            None
        }
    }

    fn config(&self) -> serde_json::Value {
        serde_json::json!({
            "port": self.port,
            "instrumentation_key": self.instrumentation_key,
        })
    }
}

// ------------------------------------------------------- ingestion listener

/// This instance's own dedicated HTTP listener speaking the Breeze ingestion protocol -
/// entirely separate from the dashboard's data API (see [`router`] below), since real SDKs
/// dial this port directly via the connection string's `IngestionEndpoint`. Also answers
/// OTLP/HTTP's standard paths (`/v1/logs`, `/v1/traces`, `/v1/metrics`) on the same port, for
/// apps instrumented with the OpenTelemetry SDK instead of the classic Application Insights
/// SDK (e.g. .NET Aspire service-defaults projects) - see [`otlp_logs`] for the JSON-only
/// caveat.
fn ingestion_router(store: TelemetryStore) -> Router {
    Router::new()
        .route("/v2/track", post(track))
        .route("/v2.1/track", post(track))
        .route("/v1/logs", post(otlp_logs))
        .route("/v1/traces", post(otlp_traces))
        .route("/v1/metrics", post(otlp_metrics))
        // Real SDKs also probe a QuickPulse ("Live Metrics") endpoint under the same
        // `LiveEndpoint` host - full QuickPulse support is out of scope for this emulator, so
        // this permissive fallback just answers 200 to anything else instead of the SDK
        // logging noisy connection-refused/404 warnings for a feature that was never
        // implemented in the first place.
        .fallback(|| async { StatusCode::OK })
        .layer(CorsLayer::permissive())
        .with_state(store)
}

#[derive(Serialize)]
struct TrackResponse {
    #[serde(rename = "itemsReceived")]
    items_received: usize,
    #[serde(rename = "itemsAccepted")]
    items_accepted: usize,
    errors: Vec<serde_json::Value>,
}

/// Decodes a request body to UTF-8 text, transparently gunzipping it first if
/// `Content-Encoding: gzip` is set - shared by the Breeze `/v2/track` handler and the OTLP
/// handlers below, both of which allow (but don't require) gzip-compressed bodies.
fn decode_text_body(headers: &HeaderMap, body: &Bytes) -> Result<String, String> {
    let is_gzip = headers
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("gzip"))
        .unwrap_or(false);
    if is_gzip {
        let mut decoder = flate2::read::GzDecoder::new(&body[..]);
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .map(|_| decompressed)
            .map_err(|err| format!("failed to decompress request body: {err}"))
    } else {
        Ok(String::from_utf8_lossy(body).into_owned())
    }
}

/// Handles `POST /v2/track` (and `/v2.1/track`): the Application Insights SDKs' batch
/// telemetry upload. The body is either a JSON array of envelopes or newline-delimited JSON
/// objects, optionally gzip-compressed (`Content-Encoding: gzip`) - both the default
/// `ServerTelemetryChannel` behavior for real Azure ingestion.
async fn track(State(store): State<TelemetryStore>, headers: HeaderMap, body: Bytes) -> Json<TrackResponse> {
    let text = match decode_text_body(&headers, &body) {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!(%err, "failed to decode Application Insights ingestion payload");
            return Json(TrackResponse {
                items_received: 0,
                items_accepted: 0,
                errors: vec![serde_json::json!({ "message": err })],
            });
        }
    };

    let (received, accepted) = store.ingest_body(&text);
    Json(TrackResponse {
        items_received: received,
        items_accepted: accepted,
        errors: Vec::new(),
    })
}

/// OTLP/HTTP only supports a JSON body here (not the SDKs' default `application/
/// x-protobuf` encoding, which would require a full protobuf/prost dependency to decode) -
/// checked via `Content-Type` so a mismatched exporter gets a clear, actionable error
/// instead of a confusing parse failure. Apps must set
/// `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` to use this receiver.
fn require_otlp_json(headers: &HeaderMap) -> Option<Response> {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if content_type.starts_with("application/json") {
        return None;
    }
    Some(
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(serde_json::json!({
                "error": format!(
                    "AzLocalDev's OTLP receiver only accepts OTLP/HTTP with JSON encoding (got Content-Type '{content_type}'). \
                     Set OTEL_EXPORTER_OTLP_PROTOCOL=http/json on the exporting app (protobuf isn't supported)."
                )
            })),
        )
            .into_response(),
    )
}

/// Handles `POST /v1/logs`: an OTLP `ExportLogsServiceRequest` - the structured-logs
/// equivalent for apps instrumented with the OpenTelemetry SDK (e.g. Aspire service-defaults
/// projects) instead of the classic Application Insights SDK.
async fn otlp_logs(State(store): State<TelemetryStore>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(resp) = require_otlp_json(&headers) {
        return resp;
    }
    match decode_text_body(&headers, &body).and_then(|text| store.ingest_otlp_logs(&text)) {
        Ok(_) => Json(serde_json::json!({})).into_response(),
        Err(err) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": err }))).into_response(),
    }
}

/// Handles `POST /v1/traces`: an OTLP `ExportTraceServiceRequest`.
async fn otlp_traces(State(store): State<TelemetryStore>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(resp) = require_otlp_json(&headers) {
        return resp;
    }
    match decode_text_body(&headers, &body).and_then(|text| store.ingest_otlp_traces(&text)) {
        Ok(_) => Json(serde_json::json!({})).into_response(),
        Err(err) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": err }))).into_response(),
    }
}

/// Handles `POST /v1/metrics`: an OTLP `ExportMetricsServiceRequest`.
async fn otlp_metrics(State(store): State<TelemetryStore>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(resp) = require_otlp_json(&headers) {
        return resp;
    }
    match decode_text_body(&headers, &body).and_then(|text| store.ingest_otlp_metrics(&text)) {
        Ok(_) => Json(serde_json::json!({})).into_response(),
        Err(err) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": err }))).into_response(),
    }
}

// --------------------------------------------------------------- web routes

/// Thread-safe lookup table of `instance id -> AppInsightsEngine`, used by this module's
/// axum routes to resolve which instance a request is for (see [`router`]). Kept separate
/// from the generic [`emu_registry::EngineRegistry`] so route handlers can call
/// `AppInsightsEngine`-specific methods directly without downcasting.
#[derive(Clone, Default)]
pub struct AppInsightsRegistry {
    inner: Arc<StdMutex<HashMap<String, Arc<AppInsightsEngine>>>>,
}

impl AppInsightsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, engine: Arc<AppInsightsEngine>) {
        self.inner.lock().unwrap().insert(engine.id().to_string(), engine);
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    pub fn get(&self, id: &str) -> Option<Arc<AppInsightsEngine>> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    /// Every currently-registered Application Insights instance - used at startup to bump
    /// the dashboard's port counter past whatever was just restored from a saved group, so
    /// newly-created instances afterward can't collide with restored ones.
    pub fn all(&self) -> Vec<Arc<AppInsightsEngine>> {
        self.inner.lock().unwrap().values().cloned().collect()
    }
}

fn require_engine(registry: &AppInsightsRegistry, id: &str) -> Result<Arc<AppInsightsEngine>, (StatusCode, String)> {
    registry
        .get(id)
        .ok_or((StatusCode::NOT_FOUND, format!("unknown Application Insights instance '{id}'")))
}

#[derive(Serialize)]
struct StatsEntry {
    #[serde(rename = "type")]
    item_type: TelemetryType,
    count: usize,
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(rename = "type")]
    item_type: Option<String>,
}

fn parse_type_filter(raw: Option<&str>) -> Option<TelemetryType> {
    let raw = raw?;
    TelemetryType::all().into_iter().find(|t| t.as_str() == raw)
}

/// This module's own axum router (captured-telemetry data API), keyed by instance id so the
/// dashboard UI can address any number of Application Insights instances the user has
/// created. The dashboard UI mounts this under a path prefix (e.g. `/api/app-insights`) -
/// route paths here are relative to that.
pub fn router(registry: AppInsightsRegistry) -> Router {
    Router::new()
        .route("/:id/items", get(list_items).delete(clear_items))
        .route("/:id/stats", get(stats))
        .route("/:id/traces", get(list_traces))
        .route("/:id/traces/:operation_id", get(get_trace))
        .route("/:id/logs", get(list_logs))
        .with_state(registry)
}

async fn list_items(
    State(registry): State<AppInsightsRegistry>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<TelemetryItem>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    let filter = parse_type_filter(query.item_type.as_deref());
    Ok(Json(store.list(filter)))
}

async fn clear_items(
    State(registry): State<AppInsightsRegistry>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    store.clear();
    Ok(StatusCode::NO_CONTENT)
}

async fn stats(
    State(registry): State<AppInsightsRegistry>,
    Path(id): Path<String>,
) -> Result<Json<Vec<StatsEntry>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(
        store
            .stats()
            .into_iter()
            .map(|(item_type, count)| StatsEntry { item_type, count })
            .collect(),
    ))
}

/// Aspire-dashboard-style "Traces" tab: every distributed operation captured so far,
/// collapsed into one row each (see `TelemetryStore::list_traces`).
async fn list_traces(
    State(registry): State<AppInsightsRegistry>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TraceSummary>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.list_traces()))
}

/// One trace's full waterfall: every request/dependency/exception/trace-message sharing the
/// given `operation_id` (or synthetic `standalone-{id}` key), oldest first.
async fn get_trace(
    State(registry): State<AppInsightsRegistry>,
    Path((id, operation_id)): Path<(String, String)>,
) -> Result<Json<Vec<TelemetryItem>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.get_trace(&operation_id)))
}

#[derive(Deserialize)]
struct LogsQuery {
    severity: Option<i64>,
    search: Option<String>,
}

/// Aspire-dashboard-style "Structured Logs" tab: every captured trace-message (`TrackTrace`/
/// `ILogger` equivalent), optionally filtered by exact severity level and/or a search term.
async fn list_logs(
    State(registry): State<AppInsightsRegistry>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Vec<TelemetryItem>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.list_logs(query.severity, query.search.as_deref())))
}
