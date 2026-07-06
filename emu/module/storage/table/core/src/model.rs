//! Serializable view types for the dashboard/API and the OData JSON wire protocol.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Map;

/// Dashboard/API view of one table (no entity contents).
#[derive(Debug, Clone, Serialize)]
pub struct TableSummary {
    pub name: String,
    pub entity_count: usize,
}

/// One entity, as returned by get/query. `properties` holds every custom property exactly
/// as the client sent it (including any `@odata.type` type-annotation keys for non-string
/// EDM types) - this emulator round-trips property bags as opaque JSON rather than
/// interpreting/validating the EDM type system, so it accepts whatever shape a real
/// `Azure.Data.Tables` client sends.
#[derive(Debug, Clone, Serialize)]
pub struct EntityView {
    pub partition_key: String,
    pub row_key: String,
    pub timestamp: DateTime<Utc>,
    pub etag: String,
    pub properties: Map<String, serde_json::Value>,
}

/// Whole-store snapshot, serialized alongside the Blob/Queue dumps in this instance's
/// `%APPDATA%/AzLocalDev/data/storage-blob/{id}.json` file (see `emu-storage-blob-engine`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableStoreDump {
    pub tables: Vec<TableDump>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDump {
    pub name: String,
    pub entities: Vec<EntityDump>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityDump {
    pub partition_key: String,
    pub row_key: String,
    pub timestamp: DateTime<Utc>,
    pub etag: String,
    pub properties: Map<String, serde_json::Value>,
}

