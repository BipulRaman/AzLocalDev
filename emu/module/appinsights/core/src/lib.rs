//! Core domain model for the Application Insights (telemetry ingestion) emulator.
//!
//! This crate has no networking/protocol code in it at all - it is a plain thread-safe
//! in-memory store of captured telemetry, plus the Breeze envelope parsing logic. The wire
//! protocol (`POST /v2/track`, gzip handling, HTTP listener) lives in
//! `emu-appinsights-engine`, driven purely by this store's methods.

mod model;
mod otlp;

pub use model::{parse_envelope, TelemetryItem, TelemetryType, TraceSummary};
pub use otlp::{parse_otlp_logs, parse_otlp_metrics, parse_otlp_traces};

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Caps how many telemetry items a single instance keeps in memory - captured telemetry is
/// transient debugging data (unlike a Storage Blob's or Service Bus queue's actual
/// payloads), so it intentionally isn't persisted to disk and isn't kept forever; oldest
/// items are dropped once this limit is hit rather than letting a chatty app grow the
/// emulator's memory use without bound.
const MAX_ITEMS: usize = 5_000;

struct StoreState {
    items: VecDeque<TelemetryItem>,
}

/// Thread-safe, cheaply-cloneable store of every telemetry item captured by one
/// Application Insights emulator instance. All clones share the same underlying state.
#[derive(Clone)]
pub struct TelemetryStore {
    state: Arc<Mutex<StoreState>>,
    next_id: Arc<AtomicU64>,
}

impl Default for TelemetryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryStore {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(StoreState { items: VecDeque::new() })),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Parses `body` as either a JSON array of envelopes or newline-delimited JSON objects
    /// (the two shapes real Application Insights SDKs actually send to `/v2/track`) and
    /// ingests every envelope found. Returns `(received, accepted)` counts for the Breeze
    /// protocol's response body - malformed individual lines/entries are skipped rather
    /// than failing the whole batch, matching the real service's lenient behavior.
    pub fn ingest_body(&self, body: &str) -> (usize, usize) {
        let envelopes = parse_batch(body);
        let received = envelopes.len();
        let mut accepted = 0;
        for envelope in &envelopes {
            if let Some(mut item) = parse_envelope(envelope) {
                item.id = self.next_id.fetch_add(1, Ordering::SeqCst);
                self.push(item);
                accepted += 1;
            }
        }
        (received, accepted)
    }

    /// Parses `body` as an OTLP (OpenTelemetry Protocol) JSON `ExportLogsServiceRequest`
    /// (`POST /v1/logs`) and ingests every log record found. Returns the number of items
    /// ingested, or an error string if `body` isn't valid JSON at all.
    pub fn ingest_otlp_logs(&self, body: &str) -> Result<usize, String> {
        self.ingest_otlp(body, otlp::parse_otlp_logs)
    }

    /// Parses `body` as an OTLP JSON `ExportTraceServiceRequest` (`POST /v1/traces`) and
    /// ingests every span found. Returns the number of items ingested, or an error string
    /// if `body` isn't valid JSON at all.
    pub fn ingest_otlp_traces(&self, body: &str) -> Result<usize, String> {
        self.ingest_otlp(body, otlp::parse_otlp_traces)
    }

    /// Parses `body` as an OTLP JSON `ExportMetricsServiceRequest` (`POST /v1/metrics`) and
    /// ingests every data point found. Returns the number of items ingested, or an error
    /// string if `body` isn't valid JSON at all.
    pub fn ingest_otlp_metrics(&self, body: &str) -> Result<usize, String> {
        self.ingest_otlp(body, otlp::parse_otlp_metrics)
    }

    fn ingest_otlp(&self, body: &str, parse: impl Fn(&serde_json::Value) -> Vec<TelemetryItem>) -> Result<usize, String> {
        let root: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        let mut items = parse(&root);
        let count = items.len();
        for item in &mut items {
            item.id = self.next_id.fetch_add(1, Ordering::SeqCst);
        }
        for item in items {
            self.push(item);
        }
        Ok(count)
    }

    fn push(&self, item: TelemetryItem) {
        let mut state = self.state.lock().unwrap();
        state.items.push_back(item);
        while state.items.len() > MAX_ITEMS {
            state.items.pop_front();
        }
    }

    /// Lists captured items, newest first, optionally filtered to one [`TelemetryType`].
    pub fn list(&self, filter: Option<TelemetryType>) -> Vec<TelemetryItem> {
        let state = self.state.lock().unwrap();
        state
            .items
            .iter()
            .rev()
            .filter(|item| filter.is_none_or(|f| item.item_type == f))
            .cloned()
            .collect()
    }

    /// Count of captured items per telemetry type, for the dashboard's per-type tab counts.
    pub fn stats(&self) -> Vec<(TelemetryType, usize)> {
        let state = self.state.lock().unwrap();
        TelemetryType::all()
            .into_iter()
            .map(|t| (t, state.items.iter().filter(|item| item.item_type == t).count()))
            .collect()
    }

    /// Groups requests/dependencies/exceptions/trace-messages by
    /// [`TelemetryItem::operation_id`] into Aspire-dashboard-style trace summaries, newest
    /// first. Items with no `operation_id` at all (an SDK that doesn't stamp correlation
    /// tags) each become their own single-item trace, keyed by their own store id, rather
    /// than being silently dropped from the view.
    pub fn list_traces(&self) -> Vec<TraceSummary> {
        let state = self.state.lock().unwrap();
        let mut groups: HashMap<String, Vec<&TelemetryItem>> = HashMap::new();
        for item in state.items.iter() {
            if !matches!(
                item.item_type,
                TelemetryType::Request | TelemetryType::Dependency | TelemetryType::Exception | TelemetryType::Trace
            ) {
                continue;
            }
            let key = item.operation_id.clone().unwrap_or_else(|| standalone_key(item.id));
            groups.entry(key).or_default().push(item);
        }
        let mut summaries: Vec<TraceSummary> = groups.into_iter().map(|(id, items)| build_trace_summary(id, &items)).collect();
        summaries.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        summaries
    }

    /// Every telemetry item belonging to one trace (`operation_id`), sorted oldest first for
    /// waterfall rendering. Accepts either a real `operation_id` or the synthetic
    /// `standalone-{id}` key [`TelemetryStore::list_traces`] assigns to correlation-less items.
    pub fn get_trace(&self, operation_id: &str) -> Vec<TelemetryItem> {
        let state = self.state.lock().unwrap();
        let mut items: Vec<TelemetryItem> = state
            .items
            .iter()
            .filter(|item| match &item.operation_id {
                Some(id) => id == operation_id,
                None => standalone_key(item.id) == operation_id,
            })
            .cloned()
            .collect();
        items.sort_by_key(|item| item.time.unwrap_or(item.received_at));
        items
    }

    /// Structured-logs view: every [`TelemetryType::Trace`] item (the emulator's analog of
    /// `ILogger`/structured log output), newest first, optionally filtered by exact
    /// `SeverityLevel` and/or a case-insensitive substring match on the message.
    pub fn list_logs(&self, severity: Option<i64>, search: Option<&str>) -> Vec<TelemetryItem> {
        let state = self.state.lock().unwrap();
        let query = search.map(|s| s.to_lowercase());
        state
            .items
            .iter()
            .rev()
            .filter(|item| item.item_type == TelemetryType::Trace)
            .filter(|item| severity.is_none_or(|s| item.severity == Some(s)))
            .filter(|item| query.as_deref().is_none_or(|q| item.name.to_lowercase().contains(q)))
            .cloned()
            .collect()
    }

    pub fn total(&self) -> usize {
        self.state.lock().unwrap().items.len()
    }

    pub fn clear(&self) {
        self.state.lock().unwrap().items.clear();
    }
}

/// Parses an ingestion request body as a batch of envelopes: tries a JSON array first,
/// falling back to newline-delimited JSON objects (the Breeze protocol's usual shape).
fn parse_batch(body: &str) -> Vec<serde_json::Value> {
    let trimmed = body.trim();
    if trimmed.starts_with('[') {
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str(trimmed) {
            return items;
        }
    }
    trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Synthetic trace key assigned to a telemetry item that has no `operation_id` at all, so
/// it still gets its own row in the trace list instead of being dropped.
fn standalone_key(item_id: u64) -> String {
    format!("standalone-{item_id}")
}

/// Collapses every telemetry item sharing one `operation_id` into a single
/// [`TraceSummary`] row: picks the root request (or, failing that, the earliest span) as
/// the trace's display name, spans the start/end time across every item's own
/// timestamp+duration, and flags the trace as errored if any exception was captured or any
/// request/dependency reported failure.
fn build_trace_summary(operation_id: String, items: &[&TelemetryItem]) -> TraceSummary {
    // Deliberately NOT clamped to a minimum of 1 - a trace with zero request/dependency
    // spans (a standalone log/exception with nothing to waterfall) needs to report a real
    // `0` here, since the dashboard uses this count to decide whether to open the (mostly
    // empty) trace-detail waterfall page at all or jump straight to the item's own details.
    let span_count = items
        .iter()
        .filter(|item| matches!(item.item_type, TelemetryType::Request | TelemetryType::Dependency))
        .count();
    let has_error = items
        .iter()
        .any(|item| item.item_type == TelemetryType::Exception || item.success == Some(false));

    let root = items
        .iter()
        .find(|item| item.item_type == TelemetryType::Request)
        .or_else(|| items.iter().min_by_key(|item| item.time.unwrap_or(item.received_at)))
        .copied();
    let root_name = root
        .map(|item| item.name.clone())
        .or_else(|| items.iter().find_map(|item| item.operation_name.clone()))
        .unwrap_or_else(|| "(unnamed operation)".to_string());

    let start_time = items
        .iter()
        .map(|item| item.time.unwrap_or(item.received_at))
        .min()
        .unwrap_or_else(chrono::Utc::now);
    let end_time = items
        .iter()
        .map(|item| {
            let started = item.time.unwrap_or(item.received_at);
            started + chrono::Duration::milliseconds(item.duration_ms.unwrap_or(0.0) as i64)
        })
        .max()
        .unwrap_or(start_time);
    let duration_ms = (end_time - start_time).num_milliseconds().max(0) as f64;

    TraceSummary {
        operation_id,
        root_name,
        start_time,
        duration_ms,
        span_count,
        has_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingests_ndjson_batch() {
        let store = TelemetryStore::new();
        let body = r#"{"name":"Microsoft.ApplicationInsights.Event.Request","time":"2024-01-01T00:00:00.0000000Z","iKey":"abc","tags":{},"data":{"baseType":"RequestData","baseData":{"name":"GET /","success":true,"responseCode":"200","duration":"00:00:00.1000000"}}}"#;
        let (received, accepted) = store.ingest_body(body);
        assert_eq!(received, 1);
        assert_eq!(accepted, 1);
        let items = store.list(Some(TelemetryType::Request));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "GET /");
        assert_eq!(items[0].duration_ms, Some(100.0));
    }

    #[test]
    fn caps_at_max_items() {
        let store = TelemetryStore::new();
        for _ in 0..(MAX_ITEMS + 10) {
            store.ingest_body(r#"{"name":"x","data":{"baseType":"EventData","baseData":{"name":"e"}}}"#);
        }
        assert_eq!(store.total(), MAX_ITEMS);
    }

    #[test]
    fn groups_request_and_dependency_into_one_trace() {
        let store = TelemetryStore::new();
        store.ingest_body(
            r#"{"name":"Request","time":"2024-01-01T00:00:00.0000000Z","tags":{"ai.operation.id":"op1"},"data":{"baseType":"RequestData","baseData":{"id":"span1","name":"GET /orders","success":true,"duration":"00:00:01.0000000"}}}"#,
        );
        store.ingest_body(
            r#"{"name":"Dependency","time":"2024-01-01T00:00:00.2000000Z","tags":{"ai.operation.id":"op1","ai.operation.parentId":"span1"},"data":{"baseType":"RemoteDependencyData","baseData":{"id":"span2","name":"SQL SELECT","success":true,"duration":"00:00:00.3000000"}}}"#,
        );

        let traces = store.list_traces();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].operation_id, "op1");
        assert_eq!(traces[0].root_name, "GET /orders");
        assert_eq!(traces[0].span_count, 2);
        assert!(!traces[0].has_error);

        let spans = store.get_trace("op1");
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn filters_structured_logs_by_severity_and_search() {
        let store = TelemetryStore::new();
        store.ingest_body(
            r#"{"name":"Trace","data":{"baseType":"MessageData","baseData":{"message":"starting up","severityLevel":1}}}"#,
        );
        store.ingest_body(
            r#"{"name":"Trace","data":{"baseType":"MessageData","baseData":{"message":"boom","severityLevel":3}}}"#,
        );

        assert_eq!(store.list_logs(None, None).len(), 2);
        assert_eq!(store.list_logs(Some(3), None).len(), 1);
        assert_eq!(store.list_logs(None, Some("boom")).len(), 1);
        assert_eq!(store.list_logs(None, Some("nope")).len(), 0);
    }
}
