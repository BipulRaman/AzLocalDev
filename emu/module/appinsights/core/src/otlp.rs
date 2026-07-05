//! Parses OTLP (OpenTelemetry Protocol) JSON payloads - the format the Aspire dashboard and
//! any OpenTelemetry SDK exporter configured with `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`
//! send - into the same [`TelemetryItem`] shape used for Breeze-ingested telemetry, so the
//! dashboard's existing Traces/Structured Logs/Metrics views work unchanged regardless of
//! which protocol an app is instrumented with.
//!
//! Only the JSON encoding of OTLP/HTTP is supported (not protobuf, which is what OTel SDKs
//! default to unless `OTEL_EXPORTER_OTLP_PROTOCOL` is explicitly set to `http/json`) - see
//! `emu-appinsights-engine`'s OTLP routes for the content-type check and the resulting error
//! message telling the caller how to switch their SDK to JSON.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::{TelemetryItem, TelemetryType};

/// Maps an OTel `SeverityNumber` (1-24, see the OpenTelemetry logs data model) down to our
/// 5-level scale (0=Trace/Verbose, 1=Information, 2=Warning, 3=Error, 4=Critical) - the same
/// bucketing the Application Insights Log Exporter for OpenTelemetry uses.
fn map_otel_severity(n: i64) -> i64 {
    match n {
        1..=8 => 0,
        9..=12 => 1,
        13..=16 => 2,
        17..=20 => 3,
        21..=24 => 4,
        _ => 1,
    }
}

/// Converts an OTLP `AnyValue` JSON object (`{"stringValue": ...}` / `{"intValue": ...}` /
/// `{"arrayValue": {"values": [...]}}` / etc.) into a plain `serde_json::Value` - unwrapping
/// the type-tagged wrapper protobuf's JSON mapping uses so the dashboard doesn't need to
/// know about OTLP's `AnyValue` shape at all.
fn any_value_to_json(v: &Value) -> Value {
    let Some(obj) = v.as_object() else { return v.clone() };
    if let Some(s) = obj.get("stringValue") {
        return s.clone();
    }
    if let Some(b) = obj.get("boolValue") {
        return b.clone();
    }
    if let Some(i) = obj.get("intValue") {
        // Protobuf's JSON mapping encodes int64 as a JSON string (to avoid precision loss in
        // JS numbers) - decode it back to a real number for display/storage.
        if let Some(s) = i.as_str() {
            if let Ok(n) = s.parse::<i64>() {
                return Value::from(n);
            }
        }
        return i.clone();
    }
    if let Some(d) = obj.get("doubleValue") {
        return d.clone();
    }
    if let Some(bytes) = obj.get("bytesValue") {
        return bytes.clone();
    }
    if let Some(arr) = obj.get("arrayValue").and_then(|a| a.get("values")).and_then(|v| v.as_array()) {
        return Value::Array(arr.iter().map(any_value_to_json).collect());
    }
    if let Some(kvlist) = obj.get("kvlistValue").and_then(|k| k.get("values")).and_then(|v| v.as_array()) {
        return Value::Object(attrs_to_map(kvlist));
    }
    Value::Null
}

/// Converts an OTLP attributes array (`[{"key": "...", "value": <AnyValue>}, ...]`) - used
/// for both resource/scope/log/span attributes and OTLP's own `kvlistValue` - into a plain
/// name -> value map.
fn attrs_to_map(attrs: &[Value]) -> Map<String, Value> {
    let mut map = Map::new();
    for attr in attrs {
        let Some(key) = attr.get("key").and_then(|v| v.as_str()) else { continue };
        let value = attr.get("value").map(any_value_to_json).unwrap_or(Value::Null);
        map.insert(key.to_string(), value);
    }
    map
}

/// Builds our AI-tag-shaped resource info (`ai.cloud.role`/`ai.cloud.roleInstance`/
/// `ai.internal.sdkVersion`) from an OTLP `Resource`'s attributes (`service.name`/
/// `service.instance.id`/`telemetry.sdk.*`) - reusing these exact tag keys means the
/// dashboard's existing "Resource" panel section (built for Breeze telemetry) displays
/// OTLP-sourced items correctly with zero frontend changes.
fn resource_tags(resource: Option<&Value>) -> Map<String, Value> {
    let mut tags = Map::new();
    let Some(attrs) = resource.and_then(|r| r.get("attributes")).and_then(|v| v.as_array()) else {
        return tags;
    };
    let map = attrs_to_map(attrs);
    if let Some(v) = map.get("service.name") {
        tags.insert("ai.cloud.role".to_string(), v.clone());
    }
    if let Some(v) = map.get("service.instance.id") {
        tags.insert("ai.cloud.roleInstance".to_string(), v.clone());
    }
    let sdk_version = map.get("telemetry.sdk.version").and_then(|v| v.as_str()).unwrap_or_default();
    if !sdk_version.is_empty() {
        let lang = map.get("telemetry.sdk.language").and_then(|v| v.as_str()).unwrap_or_default();
        let sdk_name = map.get("telemetry.sdk.name").and_then(|v| v.as_str()).unwrap_or_default();
        tags.insert("ai.internal.sdkVersion".to_string(), Value::String(format!("otel:{lang}:{sdk_name}:{sdk_version}")));
    }
    tags
}

/// Parses a `timeUnixNano`/`startTimeUnixNano`/etc. field - nanoseconds since the Unix
/// epoch, encoded as a JSON string (protobuf's `fixed64`/`uint64` JSON mapping) - into a
/// [`DateTime<Utc>`].
fn nanos_to_datetime(s: &str) -> Option<DateTime<Utc>> {
    let nanos: i64 = s.parse().ok()?;
    let secs = nanos.div_euclid(1_000_000_000);
    let nanosub = nanos.rem_euclid(1_000_000_000) as u32;
    DateTime::from_timestamp(secs, nanosub)
}

/// Decodes an OTLP trace/span id - base64-encoded bytes (protobuf's `bytes` JSON mapping) -
/// into a lowercase hex string (the conventional human-readable form). Returns `None` for an
/// all-zero id, which per the OTel spec means "unset"/absent rather than a real id.
fn id_to_hex(b64: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if bytes.is_empty() || bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Parses an OTLP `ExportLogsServiceRequest` JSON body (`POST /v1/logs`) into
/// [`TelemetryItem`]s of type [`TelemetryType::Trace`] - the emulator's structured-logs
/// equivalent, matching how the Application Insights OpenTelemetry exporter/Aspire dashboard
/// both treat OTel log records as the "Structured Logs" data source.
pub fn parse_otlp_logs(root: &Value) -> Vec<TelemetryItem> {
    let mut out = Vec::new();
    let Some(resource_logs) = root.get("resourceLogs").and_then(|v| v.as_array()) else {
        return out;
    };
    for rl in resource_logs {
        let tags = resource_tags(rl.get("resource"));
        let Some(scope_logs) = rl.get("scopeLogs").and_then(|v| v.as_array()) else { continue };
        for sl in scope_logs {
            let scope_name = sl.get("scope").and_then(|s| s.get("name")).and_then(|v| v.as_str()).map(str::to_string);
            let Some(records) = sl.get("logRecords").and_then(|v| v.as_array()) else { continue };
            for rec in records {
                let time = rec
                    .get("timeUnixNano")
                    .and_then(|v| v.as_str())
                    .and_then(nanos_to_datetime)
                    .or_else(|| rec.get("observedTimeUnixNano").and_then(|v| v.as_str()).and_then(nanos_to_datetime));
                let severity_number = rec.get("severityNumber").and_then(|v| v.as_i64()).unwrap_or(9);
                let body = rec.get("body").map(any_value_to_json).unwrap_or(Value::Null);
                let name = match &body {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                let properties = rec.get("attributes").and_then(|v| v.as_array()).map(|a| attrs_to_map(a)).unwrap_or_default();
                let operation_id = rec.get("traceId").and_then(|v| v.as_str()).and_then(id_to_hex);

                out.push(TelemetryItem {
                    id: 0,
                    received_at: Utc::now(),
                    item_type: TelemetryType::Trace,
                    time,
                    ikey: None,
                    name,
                    success: None,
                    response_code: None,
                    duration_ms: None,
                    severity: Some(map_otel_severity(severity_number)),
                    operation_id,
                    operation_name: scope_name.clone(),
                    span_id: None,
                    parent_span_id: None,
                    properties,
                    tags: tags.clone(),
                    data: rec.clone(),
                });
            }
        }
    }
    out
}

/// Parses an OTLP `ExportTraceServiceRequest` JSON body (`POST /v1/traces`) into
/// [`TelemetryItem`]s of type [`TelemetryType::Request`] (server/consumer spans) or
/// [`TelemetryType::Dependency`] (everything else) - `traceId`/`spanId`/`parentSpanId` map
/// directly onto [`TelemetryItem::operation_id`]/[`TelemetryItem::span_id`]/
/// [`TelemetryItem::parent_span_id`], so the dashboard's existing trace-waterfall grouping
/// (built for Breeze's `ai.operation.id`/`ai.operation.parentId`) works unchanged.
pub fn parse_otlp_traces(root: &Value) -> Vec<TelemetryItem> {
    let mut out = Vec::new();
    let Some(resource_spans) = root.get("resourceSpans").and_then(|v| v.as_array()) else {
        return out;
    };
    for rs in resource_spans {
        let tags = resource_tags(rs.get("resource"));
        let Some(scope_spans) = rs.get("scopeSpans").and_then(|v| v.as_array()) else { continue };
        for ss in scope_spans {
            let scope_name = ss.get("scope").and_then(|s| s.get("name")).and_then(|v| v.as_str()).map(str::to_string);
            let Some(spans) = ss.get("spans").and_then(|v| v.as_array()) else { continue };
            for span in spans {
                // OTel span kinds: 1=INTERNAL, 2=SERVER, 3=CLIENT, 4=PRODUCER, 5=CONSUMER.
                // SERVER/CONSUMER spans are the ones an operation is rooted at (equivalent
                // to a Breeze `RequestData`); everything else is a "downstream call" span
                // (equivalent to `RemoteDependencyData`).
                let kind = span.get("kind").and_then(|v| v.as_i64()).unwrap_or(0);
                let item_type = match kind {
                    2 | 5 => TelemetryType::Request,
                    _ => TelemetryType::Dependency,
                };

                let start = span.get("startTimeUnixNano").and_then(|v| v.as_str()).and_then(nanos_to_datetime);
                let end = span.get("endTimeUnixNano").and_then(|v| v.as_str()).and_then(nanos_to_datetime);
                let duration_ms = match (start, end) {
                    (Some(s), Some(e)) => Some((e - s).num_microseconds().unwrap_or(0) as f64 / 1000.0),
                    _ => None,
                };

                // OTel span status: 0=UNSET, 1=OK, 2=ERROR.
                let success = span.get("status").and_then(|s| s.get("code")).and_then(|v| v.as_i64()).map(|c| c != 2);
                let name = span.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let properties = span.get("attributes").and_then(|v| v.as_array()).map(|a| attrs_to_map(a)).unwrap_or_default();
                let response_code = properties
                    .get("http.status_code")
                    .or_else(|| properties.get("http.response.status_code"))
                    .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));

                let operation_id = span.get("traceId").and_then(|v| v.as_str()).and_then(id_to_hex);
                let span_id = span.get("spanId").and_then(|v| v.as_str()).and_then(id_to_hex);
                let parent_span_id = span.get("parentSpanId").and_then(|v| v.as_str()).and_then(id_to_hex);

                out.push(TelemetryItem {
                    id: 0,
                    received_at: Utc::now(),
                    item_type,
                    time: start,
                    ikey: None,
                    name,
                    success,
                    response_code,
                    duration_ms,
                    severity: None,
                    operation_id,
                    operation_name: scope_name.clone(),
                    span_id,
                    parent_span_id,
                    properties,
                    tags: tags.clone(),
                    data: span.clone(),
                });
            }
        }
    }
    out
}

/// Parses an OTLP `ExportMetricsServiceRequest` JSON body (`POST /v1/metrics`) into
/// [`TelemetryItem`]s of type [`TelemetryType::Metric`] - one item per data point (gauge/
/// sum/histogram data points are all treated the same way: a name + a single numeric
/// value). The `data` field is shaped exactly like a Breeze `MetricData.metrics[]` array so
/// the dashboard's existing `metricValue()` reader (in `app.js`) works unchanged.
pub fn parse_otlp_metrics(root: &Value) -> Vec<TelemetryItem> {
    let mut out = Vec::new();
    let Some(resource_metrics) = root.get("resourceMetrics").and_then(|v| v.as_array()) else {
        return out;
    };
    for rm in resource_metrics {
        let tags = resource_tags(rm.get("resource"));
        let Some(scope_metrics) = rm.get("scopeMetrics").and_then(|v| v.as_array()) else { continue };
        for sm in scope_metrics {
            let scope_name = sm.get("scope").and_then(|s| s.get("name")).and_then(|v| v.as_str()).map(str::to_string);
            let Some(metrics) = sm.get("metrics").and_then(|v| v.as_array()) else { continue };
            for metric in metrics {
                let name = metric.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let data_points = ["gauge", "sum", "histogram"]
                    .iter()
                    .find_map(|kind| metric.get(*kind).and_then(|d| d.get("dataPoints")).and_then(|v| v.as_array()));
                let Some(points) = data_points else { continue };

                for point in points {
                    let time = point.get("timeUnixNano").and_then(|v| v.as_str()).and_then(nanos_to_datetime);
                    let value = point
                        .get("asDouble")
                        .and_then(|v| v.as_f64())
                        .or_else(|| point.get("asInt").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()));
                    let properties = point.get("attributes").and_then(|v| v.as_array()).map(|a| attrs_to_map(a)).unwrap_or_default();
                    let data = serde_json::json!({ "metrics": [{ "name": name, "value": value }] });

                    out.push(TelemetryItem {
                        id: 0,
                        received_at: Utc::now(),
                        item_type: TelemetryType::Metric,
                        time,
                        ikey: None,
                        name: name.clone(),
                        success: None,
                        response_code: None,
                        duration_ms: None,
                        severity: None,
                        operation_id: None,
                        operation_name: scope_name.clone(),
                        span_id: None,
                        parent_span_id: None,
                        properties,
                        tags: tags.clone(),
                        data,
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_otlp_log_record() {
        let root: Value = serde_json::from_str(
            r#"{
              "resourceLogs": [{
                "resource": { "attributes": [
                  {"key": "service.name", "value": {"stringValue": "api"}},
                  {"key": "service.instance.id", "value": {"stringValue": "hdvbazjx"}}
                ]},
                "scopeLogs": [{
                  "scope": { "name": "DocGen.Api.Data.CosmosBootstrapper" },
                  "logRecords": [{
                    "timeUnixNano": "1783261746493527900",
                    "severityNumber": 9,
                    "severityText": "Information",
                    "body": { "stringValue": "Cosmos bootstrap complete." },
                    "attributes": [{"key": "aspire.log_id", "value": {"stringValue": "2"}}]
                  }]
                }]
              }]
            }"#,
        )
        .unwrap();

        let items = parse_otlp_logs(&root);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.item_type, TelemetryType::Trace);
        assert_eq!(item.name, "Cosmos bootstrap complete.");
        assert_eq!(item.severity, Some(1));
        assert_eq!(item.operation_name.as_deref(), Some("DocGen.Api.Data.CosmosBootstrapper"));
        assert_eq!(item.tags.get("ai.cloud.role").and_then(|v| v.as_str()), Some("api"));
        assert_eq!(item.tags.get("ai.cloud.roleInstance").and_then(|v| v.as_str()), Some("hdvbazjx"));
        assert_eq!(item.properties.get("aspire.log_id").and_then(|v| v.as_str()), Some("2"));
    }

    #[test]
    fn parses_otlp_span_kinds_and_duration() {
        let root: Value = serde_json::from_str(
            r#"{
              "resourceSpans": [{
                "resource": { "attributes": [{"key": "service.name", "value": {"stringValue": "api"}}] },
                "scopeSpans": [{
                  "spans": [{
                    "traceId": "AAAAAAAAAAAAAAAAAAAAAQ==",
                    "spanId": "AAAAAAAAAAE=",
                    "name": "GET /orders",
                    "kind": 2,
                    "startTimeUnixNano": "1000000000",
                    "endTimeUnixNano": "1500000000",
                    "status": {"code": 1}
                  }]
                }]
              }]
            }"#,
        )
        .unwrap();

        let items = parse_otlp_traces(&root);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_type, TelemetryType::Request);
        assert_eq!(items[0].name, "GET /orders");
        assert_eq!(items[0].duration_ms, Some(500.0));
        assert_eq!(items[0].success, Some(true));
        assert!(items[0].operation_id.is_some());
    }
}
