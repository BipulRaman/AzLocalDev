//! Serializable view types for the dashboard/API.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Dashboard/API view of one queue (no message contents).
#[derive(Debug, Clone, Serialize)]
pub struct QueueSummary {
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Number of messages currently visible (not counting ones hidden by an in-flight
    /// `get_messages` visibility timeout) - matches the Azure Queue REST API's
    /// `x-ms-approximate-messages-count` semantics closely enough for a dev emulator.
    pub approximate_message_count: usize,
}

/// One message as returned by `get_messages`/`peek_messages` - `pop_receipt` is `None` for a
/// peek (peeking never changes a message's state, so there's nothing to use it against).
#[derive(Debug, Clone, Serialize)]
pub struct MessageView {
    pub message_id: String,
    pub insertion_time: DateTime<Utc>,
    pub expiration_time: DateTime<Utc>,
    pub dequeue_count: u64,
    pub body: String,
    pub pop_receipt: Option<String>,
    pub time_next_visible: Option<DateTime<Utc>>,
}

/// Whole-store snapshot, serialized alongside the Blob/Table dumps in this instance's
/// `%APPDATA%/AzLocalDev/data/storage-blob/{id}.json` file (see `emu-storage-engine`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueStoreDump {
    pub queues: Vec<QueueDump>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDump {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub messages: Vec<MessageDump>,
}

/// A persisted message - deliberately has no `pop_receipt`/lease state: any in-flight lease
/// held by a real client is inherently tied to the process instance that handed it out (see
/// the crate-level doc comment), so every message always comes back from a restart fully
/// unleased (`pop_receipt: None`) regardless of whether it was leased out right before the
/// restart. `next_visible_time` (which may still be in the future from that old lease) is
/// preserved as-is, so it still won't reappear to `get_messages` any earlier than it would
/// have without a restart - just with nothing able to redeem the now-void old receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDump {
    pub id: String,
    pub body: String,
    pub insertion_time: DateTime<Utc>,
    pub expiration_time: DateTime<Utc>,
    pub next_visible_time: DateTime<Utc>,
    pub dequeue_count: u64,
}

