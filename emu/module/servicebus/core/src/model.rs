use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Which of the two Service Bus receive modes a receiver is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryMode {
    /// Message is removed from the queue as soon as it's sent to the receiver.
    ReceiveAndDelete,
    /// Message is locked (invisible to other receivers) until Complete/Abandon/DeadLetter/
    /// Defer is called, or the lock expires.
    PeekLock,
}

/// Which logical bucket a message currently lives in, inside an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageState {
    Active,
    Scheduled,
    Deferred,
    DeadLettered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKind {
    Queue,
    Subscription,
}

/// Per-entity configuration, roughly mirroring the real Service Bus entity properties
/// that actually affect emulator behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityOptions {
    pub lock_duration: chrono::Duration,
    pub max_delivery_count: u32,
    pub default_ttl: Option<chrono::Duration>,
    pub requires_session: bool,
    pub dead_letter_on_expiration: bool,
}

impl Default for EntityOptions {
    fn default() -> Self {
        Self {
            lock_duration: chrono::Duration::seconds(30),
            max_delivery_count: 10,
            default_ttl: None,
            requires_session: false,
            dead_letter_on_expiration: false,
        }
    }
}

/// A message as submitted by a sender. Not yet assigned a sequence number.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewMessage {
    pub message_id: Option<String>,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub session_id: Option<String>,
    pub partition_key: Option<String>,
    pub properties: HashMap<String, String>,
    pub scheduled_enqueue_time: Option<DateTime<Utc>>,
    pub time_to_live: Option<chrono::Duration>,
}

/// A message as stored inside an entity, once it has been assigned a sequence number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokeredMessage {
    pub sequence_number: i64,
    pub message_id: String,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub session_id: Option<String>,
    pub partition_key: Option<String>,
    pub properties: HashMap<String, String>,
    pub enqueued_time: DateTime<Utc>,
    pub scheduled_enqueue_time: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub delivery_count: u32,
    pub state: MessageState,
    /// Present only while a `PeekLock` receiver currently holds this message.
    pub lock_token: Option<Uuid>,
    pub locked_until: Option<DateTime<Utc>>,
    pub dead_letter_reason: Option<String>,
    pub dead_letter_description: Option<String>,
}

/// Snapshot of counts for one entity, used by the management/UI APIs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityStats {
    pub active_count: u64,
    pub scheduled_count: u64,
    pub deferred_count: u64,
    pub dead_letter_count: u64,
    pub requires_session: bool,
}

/// A full, serializable snapshot of one entity's (queue or subscription) durable state -
/// its configuration plus every message in every bucket. Used to persist emulator data to
/// disk so it survives the process restarting, and to restore it on the next startup.
///
/// Deliberately excludes anything tied to a live receiver connection (peek-lock tokens,
/// locked message copies, locked session ids): those can't meaningfully survive a process
/// restart since no receiver could still be holding them, so on export any currently-locked
/// message is folded back into `active` as if its lock had just expired.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityDump {
    pub options: EntityOptions,
    pub next_sequence: i64,
    pub active: Vec<BrokeredMessage>,
    pub scheduled: Vec<BrokeredMessage>,
    pub deferred: Vec<BrokeredMessage>,
    pub dead_letter: Vec<BrokeredMessage>,
}

