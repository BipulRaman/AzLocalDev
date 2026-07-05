//! Serializable view types for the dashboard/API.

use chrono::{DateTime, Utc};
use serde::Serialize;

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
