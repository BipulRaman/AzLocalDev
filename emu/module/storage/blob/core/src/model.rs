//! Serializable view types (for the dashboard/API) and the on-disk dump format used to
//! persist a [`crate::BlobStore`] across restarts.

use std::collections::HashMap;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Dashboard/API view of one container (no blob contents).
#[derive(Debug, Clone, Serialize)]
pub struct ContainerSummary {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub blob_count: usize,
}

/// Dashboard/API view of one blob's metadata (no bytes).
#[derive(Debug, Clone, Serialize)]
pub struct BlobSummary {
    pub name: String,
    pub content_type: String,
    pub content_length: u64,
    pub etag: String,
    pub last_modified: DateTime<Utc>,
}

/// A blob's full metadata + bytes, as returned by a download/get.
#[derive(Debug, Clone)]
pub struct BlobEntry {
    pub name: String,
    pub content_type: String,
    pub etag: String,
    pub last_modified: DateTime<Utc>,
    pub metadata: HashMap<String, String>,
    pub data: Bytes,
}

/// Whole-store snapshot, serialized to this instance's `%APPDATA%/EmuEngine/data/...json`
/// file. Blob bytes are base64-encoded so the whole thing round-trips through `serde_json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoreDump {
    pub containers: Vec<ContainerDump>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerDump {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub blobs: Vec<BlobDump>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobDump {
    pub name: String,
    pub content_type: String,
    pub etag: String,
    pub last_modified: DateTime<Utc>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    #[serde(with = "base64_bytes")]
    pub data: Bytes,
}

/// (De)serializes a `Bytes` field as a base64 string instead of serde's default byte-array
/// representation, so persisted dumps stay compact JSON strings.
mod base64_bytes {
    use base64::Engine;
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)?;
        Ok(Bytes::from(decoded))
    }
}
