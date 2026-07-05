//! Serializable telemetry types (for the dashboard/API) and the Breeze/Application Insights
//! ingestion envelope shapes used to parse incoming telemetry payloads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of telemetry an ingested envelope represents, derived from its `data.baseType`
/// field. Mirrors the handful of telemetry types the Application Insights SDKs actually
/// send (`Microsoft.ApplicationInsights.*Data`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryType {
    Request,
    Dependency,
    Exception,
    Trace,
    Event,
    Metric,
    PageView,
    Availability,
    Other,
}

impl TelemetryType {
    /// Maps a Breeze envelope's `data.baseType` field (e.g. `"RequestData"`) to a
    /// [`TelemetryType`]. Unrecognized/missing base types fall back to `Other` rather than
    /// being rejected outright - an unfamiliar telemetry type is still worth capturing and
    /// displaying, just without type-specific summary fields.
    pub fn from_base_type(base_type: &str) -> Self {
        match base_type {
            "RequestData" => Self::Request,
            "RemoteDependencyData" => Self::Dependency,
            "ExceptionData" => Self::Exception,
            "MessageData" => Self::Trace,
            "EventData" => Self::Event,
            "MetricData" => Self::Metric,
            "PageViewData" => Self::PageView,
            "AvailabilityData" => Self::Availability,
            _ => Self::Other,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Dependency => "dependency",
            Self::Exception => "exception",
            Self::Trace => "trace",
            Self::Event => "event",
            Self::Metric => "metric",
            Self::PageView => "page_view",
            Self::Availability => "availability",
            Self::Other => "other",
        }
    }

    /// Every variant, in the fixed order the dashboard displays telemetry-type tabs in.
    pub fn all() -> [Self; 8] {
        [
            Self::Request,
            Self::Dependency,
            Self::Exception,
            Self::Trace,
            Self::Event,
            Self::Metric,
            Self::PageView,
            Self::Availability,
        ]
    }
}

/// One captured piece of telemetry, as shown/queried by the dashboard. Common summary
/// fields (`name`, `message`, `success`, `response_code`, `duration_ms`) are pulled out of
/// the type-specific `data.baseData` payload so the dashboard can render a single table
/// across every telemetry type; `raw` keeps the full envelope for a "view details" drill-down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryItem {
    /// Monotonically increasing id assigned by the store on ingestion (not part of the
    /// envelope itself) - lets the dashboard request "everything after id N" if it wants to
    /// poll incrementally later, and gives every row a stable React/DOM key today.
    pub id: u64,
    pub received_at: DateTime<Utc>,
    pub item_type: TelemetryType,
    /// The envelope's own `time` field, when present and parseable.
    pub time: Option<DateTime<Utc>>,
    pub ikey: Option<String>,
    /// Short human-readable label for the table's "Name" column - the request/dependency/
    /// event name, the trace message, or the exception's outermost exception type+message.
    pub name: String,
    pub success: Option<bool>,
    pub response_code: Option<String>,
    pub duration_ms: Option<f64>,
    /// Trace severity level (`SeverityLevel` 0-4: Verbose/Information/Warning/Error/Critical),
    /// only set for [`TelemetryType::Trace`] and [`TelemetryType::Exception`] items.
    pub severity: Option<i64>,
    /// The distributed-operation id (`tags["ai.operation.id"]`) that ties every piece of
    /// telemetry emitted while handling one logical request/operation together - the same
    /// concept as an OpenTelemetry trace id. Used to group requests/dependencies/exceptions/
    /// traces into a single waterfall, Aspire-dashboard style (see
    /// [`crate::TelemetryStore::list_traces`]).
    pub operation_id: Option<String>,
    /// The human-readable operation name (`tags["ai.operation.name"]`), e.g. `"GET /orders"` -
    /// used as a trace's display name when no root request span was captured.
    pub operation_name: Option<String>,
    /// This item's own span id - only requests and dependencies have one (`baseData.id`);
    /// everything else (traces/exceptions/events) attaches to the *current* span via
    /// [`TelemetryItem::parent_span_id`] instead of having an id of its own.
    pub span_id: Option<String>,
    /// The id of the span this item logically happened inside
    /// (`tags["ai.operation.parentId"]`) - absent for the root span of a trace.
    pub parent_span_id: Option<String>,
    /// Custom properties (`baseData.properties`, aka "custom dimensions") attached to the
    /// telemetry item - shown in the structured-logs/trace-span detail drill-down.
    #[serde(default)]
    pub properties: serde_json::Map<String, serde_json::Value>,
    pub tags: serde_json::Map<String, serde_json::Value>,
    /// The full `data.baseData` payload, for the "view details" drill-down.
    pub data: serde_json::Value,
}


/// Parses one Breeze ingestion envelope (a single JSON object from a `POST /v2/track`
/// request body) into a [`TelemetryItem`]. `id`/`received_at` are stamped by the caller
/// (the store), since they aren't part of the envelope itself. Returns `None` if `value`
/// isn't a JSON object at all (a malformed line is skipped rather than failing the whole
/// batch).
pub fn parse_envelope(value: &serde_json::Value) -> Option<TelemetryItem> {
    let obj = value.as_object()?;

    let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let ikey = obj.get("iKey").and_then(|v| v.as_str()).map(str::to_string);
    let time = obj
        .get("time")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let tags = obj
        .get("tags")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let data_obj = obj.get("data").and_then(|v| v.as_object());
    let base_type = data_obj
        .and_then(|d| d.get("baseType"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let base_data = data_obj
        .and_then(|d| d.get("baseData"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let item_type = TelemetryType::from_base_type(base_type);

    let display_name = summary_name(item_type, &base_data, &name);
    let success = base_data.get("success").and_then(|v| v.as_bool());
    let response_code = base_data
        .get("responseCode")
        .or_else(|| base_data.get("resultCode"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let duration_ms = base_data
        .get("duration")
        .and_then(|v| v.as_str())
        .and_then(parse_duration_ms);
    let severity = base_data.get("severityLevel").and_then(|v| v.as_i64());

    let operation_id = tags
        .get("ai.operation.id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let operation_name = tags
        .get("ai.operation.name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let parent_span_id = tags
        .get("ai.operation.parentId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // Only requests/dependencies carry their own span id (`baseData.id`) - everything else
    // (traces/exceptions/events) just attaches to the current span via `parent_span_id`.
    let span_id = matches!(item_type, TelemetryType::Request | TelemetryType::Dependency)
        .then(|| base_data.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .flatten();
    let properties = base_data
        .get("properties")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    Some(TelemetryItem {
        id: 0,
        received_at: Utc::now(),
        item_type,
        time,
        ikey,
        name: display_name,
        success,
        response_code,
        duration_ms,
        severity,
        operation_id,
        operation_name,
        span_id,
        parent_span_id,
        properties,
        tags,
        data: base_data,
    })
}

/// Aspire-dashboard-style summary of one distributed operation ("trace"): every request/
/// dependency/exception/trace-message telemetry item sharing the same
/// [`TelemetryItem::operation_id`], collapsed into one row for the trace list. See
/// [`crate::TelemetryStore::list_traces`].
#[derive(Debug, Clone, Serialize)]
pub struct TraceSummary {
    pub operation_id: String,
    /// The root request's name, or the operation name tag, or the earliest span's name if
    /// neither is available.
    pub root_name: String,
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    /// Number of request/dependency spans in this trace (excludes trace-messages/exceptions,
    /// which are attached as point-events rather than their own timed span).
    pub span_count: usize,
    /// Whether any exception was captured, or any request/dependency reported failure.
    pub has_error: bool,
}

/// Picks the most useful "Name" column value for each telemetry type: the operation name
/// for requests/dependencies/page views/availability, the message text for traces, the
/// outermost exception's type+message for exceptions, and the envelope name itself
/// otherwise (events, metrics, unrecognized types).
fn summary_name(item_type: TelemetryType, base_data: &serde_json::Value, envelope_name: &str) -> String {
    match item_type {
        TelemetryType::Request | TelemetryType::Dependency | TelemetryType::PageView | TelemetryType::Availability => base_data
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| envelope_name.to_string()),
        TelemetryType::Trace => base_data
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| envelope_name.to_string()),
        TelemetryType::Event => base_data
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| envelope_name.to_string()),
        // Unlike every other telemetry type, `MetricData.name` doesn't exist at the top
        // level - the actual metric name/value live in its `metrics` array (one entry per
        // measurement in the batch; the SDKs always send exactly one for a simple
        // `TrackMetric` call).
        TelemetryType::Metric => base_data
            .get("metrics")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|m| m.get("name"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| envelope_name.to_string()),
        TelemetryType::Exception => base_data
            .get("exceptions")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .map(|exc| {
                let ex_type = exc.get("typeName").and_then(|v| v.as_str()).unwrap_or("Exception");
                let message = exc.get("message").and_then(|v| v.as_str()).unwrap_or("");
                if message.is_empty() {
                    ex_type.to_string()
                } else {
                    format!("{ex_type}: {message}")
                }
            })
            .unwrap_or_else(|| envelope_name.to_string()),
        TelemetryType::Other => envelope_name.to_string(),
    }
}

/// Parses an Application Insights `duration` string - .NET `TimeSpan` format,
/// `[d.]hh:mm:ss[.fffffff]` (e.g. `"00:00:01.2345678"` or `"1.02:03:04.5000000"`) - into
/// milliseconds. Returns `None` for anything that doesn't match.
fn parse_duration_ms(s: &str) -> Option<f64> {
    let (days, rest) = match s.split_once('.') {
        // A leading "d." segment is only a day count if it's followed by another
        // "hh:mm:ss" component (i.e. `rest` still contains a ':') - otherwise the '.' just
        // introduces the fractional-seconds part of a day-less duration.
        Some((d, r)) if d.chars().all(|c| c.is_ascii_digit()) && r.contains(':') => {
            (d.parse::<f64>().ok()?, r)
        }
        _ => (0.0, s),
    };
    let mut parts = rest.splitn(3, ':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some((((days * 24.0 + hours) * 60.0 + minutes) * 60.0 + seconds) * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_duration() {
        assert_eq!(parse_duration_ms("00:00:01.5000000"), Some(1500.0));
    }

    #[test]
    fn parses_duration_with_days() {
        assert_eq!(parse_duration_ms("1.00:00:00.0000000"), Some(86_400_000.0));
    }

    #[test]
    fn classifies_base_types() {
        assert_eq!(TelemetryType::from_base_type("RequestData"), TelemetryType::Request);
        assert_eq!(TelemetryType::from_base_type("Unknown"), TelemetryType::Other);
    }
}
