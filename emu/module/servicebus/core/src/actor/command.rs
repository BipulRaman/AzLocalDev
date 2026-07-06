//! The message protocol between an [`super::EntityHandle`] and its actor task (see
//! [`super::task::spawn_entity`]) - one [`Command`] variant per operation a queue or
//! subscription supports.

use crate::error::CoreResult;
use crate::model::{BrokeredMessage, DeliveryMode, EntityDump, EntityStats, MessageState, NewMessage};
use chrono::Utc;
use tokio::sync::oneshot;
use uuid::Uuid;

/// What a receiver gets back for one message.
#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub message: BrokeredMessage,
}

/// Commands understood by an entity actor (a queue or a subscription — they behave
/// identically from the outside, the only difference is how messages get *into* them:
/// direct send for queues, topic fan-out for subscriptions).
pub enum Command {
    Send {
        msg: NewMessage,
        reply: oneshot::Sender<CoreResult<i64>>,
    },
    TryReceive {
        mode: DeliveryMode,
        reply: oneshot::Sender<CoreResult<Option<BrokeredMessage>>>,
    },
    /// Accepts a message session: `requested = Some(id)` asks for that specific session
    /// (rejected if it's already locked by another receiver), `requested = None` asks for
    /// whichever session with pending messages isn't already locked ("next available").
    /// Returns the session id that was actually locked, or `None` if none could be granted.
    AcceptSession {
        requested: Option<String>,
        reply: oneshot::Sender<Option<String>>,
    },
    /// Releases a session lock previously granted by `AcceptSession`, e.g. when the
    /// receiver link closes.
    ReleaseSession {
        session_id: String,
    },
    /// Like `TryReceive`, but only returns messages belonging to `session_id`.
    TryReceiveSession {
        session_id: String,
        mode: DeliveryMode,
        reply: oneshot::Sender<CoreResult<Option<BrokeredMessage>>>,
    },
    Complete {
        lock_token: Uuid,
        reply: oneshot::Sender<CoreResult<()>>,
    },
    Abandon {
        lock_token: Uuid,
        reply: oneshot::Sender<CoreResult<()>>,
    },
    DeadLetter {
        lock_token: Uuid,
        reason: Option<String>,
        description: Option<String>,
        reply: oneshot::Sender<CoreResult<()>>,
    },
    Defer {
        lock_token: Uuid,
        reply: oneshot::Sender<CoreResult<()>>,
    },
    RenewLock {
        lock_token: Uuid,
        reply: oneshot::Sender<CoreResult<chrono::DateTime<Utc>>>,
    },
    Peek {
        state: MessageState,
        from_sequence: i64,
        max_count: u32,
        reply: oneshot::Sender<Vec<BrokeredMessage>>,
    },
    Purge {
        reply: oneshot::Sender<u64>,
    },
    /// Permanently removes a single message (whichever bucket it's currently in) by its
    /// sequence number. Returns whether a message was actually found and removed.
    Delete {
        sequence_number: i64,
        reply: oneshot::Sender<bool>,
    },
    /// Moves a dead-lettered message back into the active queue as a brand new message
    /// (new sequence number, delivery count reset to 0, no dead-letter reason/description),
    /// mirroring what a client would do by hand-resubmitting a DLQ message. Returns the new
    /// sequence number, or `SequenceNotFound` if `sequence_number` isn't currently in the
    /// dead-letter bucket.
    Resubmit {
        sequence_number: i64,
        reply: oneshot::Sender<CoreResult<i64>>,
    },
    Stats {
        reply: oneshot::Sender<EntityStats>,
    },
    /// Returns a full, serializable snapshot of this entity's durable state (options +
    /// every message in every bucket), for persisting to disk.
    Export {
        reply: oneshot::Sender<EntityDump>,
    },
    /// Replaces this entity's durable state with a previously-exported snapshot, e.g. when
    /// restoring persisted data on startup. Clears any in-progress locks/session locks,
    /// since no live receiver could still be holding them.
    Restore {
        dump: EntityDump,
        reply: oneshot::Sender<()>,
    },
    /// Internal tick used to expire locks/TTL/promote scheduled messages.
    Tick,
}
