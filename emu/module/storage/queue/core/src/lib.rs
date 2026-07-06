//! Core domain model for the Azure Queue Storage emulator.
//!
//! This crate has no networking/protocol code in it at all - it is a plain thread-safe
//! in-memory store of queues and messages. The wire protocol (Azure Queue REST API over
//! HTTP) lives in `emu-storage-queue-server`, driven purely by this store's methods.
//!
//! Queue/message contents ARE persisted to disk across restarts, alongside Blob data (see
//! `emu-storage-engine`'s unified `StorageEngine::start`/`stop`/autosave, which calls
//! this store's [`QueueStore::dump`]/[`QueueStore::restore`]). The one thing that does NOT
//! survive a restart is an in-flight lease (`pop_receipt`) - it's inherently tied to a single
//! process's lifetime (real Azure Queue Storage doesn't survive a client crash mid-lease
//! either, beyond the visibility timeout) - see [`model::MessageDump`]'s doc comment.

mod error;
mod model;

pub use error::{CoreError, CoreResult};
pub use model::{MessageDump, MessageView, QueueDump, QueueStoreDump, QueueSummary};

use std::sync::{Arc, Mutex as StdMutex};

use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use uuid::Uuid;

/// Default message time-to-live when the caller doesn't specify one (7 days, matching real
/// Azure Queue Storage's default).
const DEFAULT_TTL_SECS: i64 = 7 * 24 * 3600;

struct Message {
    id: String,
    body: String,
    insertion_time: DateTime<Utc>,
    expiration_time: DateTime<Utc>,
    /// When this message becomes visible for the next `get_messages` call - either just
    /// after insertion (immediately visible) or after an in-flight lease's visibility
    /// timeout expires.
    next_visible_time: DateTime<Utc>,
    dequeue_count: u64,
    /// `Some(receipt)` while a `get_messages` caller has it leased out (i.e. currently
    /// invisible); `None` once visible/available again. Required to match on
    /// `delete_message`/`update_message` - matches real Azure Queue Storage's optimistic
    /// concurrency model for in-flight messages.
    pop_receipt: Option<String>,
}

impl Message {
    fn to_view(&self, pop_receipt: Option<String>, time_next_visible: Option<DateTime<Utc>>) -> MessageView {
        MessageView {
            message_id: self.id.clone(),
            insertion_time: self.insertion_time,
            expiration_time: self.expiration_time,
            dequeue_count: self.dequeue_count,
            body: self.body.clone(),
            pop_receipt,
            time_next_visible,
        }
    }
}

struct QueueState {
    created_at: DateTime<Utc>,
    messages: StdMutex<Vec<Message>>,
}

/// Thread-safe, cheaply-cloneable store of every queue/message for one Storage account
/// emulator instance. All clones share the same underlying state.
#[derive(Clone, Default)]
pub struct QueueStore {
    queues: Arc<DashMap<String, QueueState>>,
}

impl QueueStore {
    pub fn new() -> Self {
        Self::default()
    }

    // ----------------------------------------------------------------- queues

    pub fn create_queue(&self, name: &str) -> CoreResult<()> {
        if self.queues.contains_key(name) {
            return Err(CoreError::QueueAlreadyExists(name.to_string()));
        }
        self.queues.insert(
            name.to_string(),
            QueueState {
                created_at: Utc::now(),
                messages: StdMutex::new(Vec::new()),
            },
        );
        Ok(())
    }

    pub fn queue_exists(&self, name: &str) -> bool {
        self.queues.contains_key(name)
    }

    pub fn delete_queue(&self, name: &str) -> CoreResult<()> {
        self.queues.remove(name).map(|_| ()).ok_or_else(|| CoreError::QueueNotFound(name.to_string()))
    }

    pub fn list_queues(&self) -> Vec<QueueSummary> {
        let now = Utc::now();
        self.queues
            .iter()
            .map(|entry| {
                let messages = entry.value().messages.lock().unwrap();
                let visible = messages.iter().filter(|m| m.next_visible_time <= now && m.expiration_time > now).count();
                QueueSummary {
                    name: entry.key().clone(),
                    created_at: entry.value().created_at,
                    approximate_message_count: visible,
                }
            })
            .collect()
    }

    // --------------------------------------------------------------- messages

    /// Enqueues a new message. `visibility_timeout_secs` delays when it first becomes
    /// visible to `get_messages` (0 = immediately visible, the common case);
    /// `ttl_secs` is `None` for the default 7-day expiry, or `Some(-1)` for "never expires".
    pub fn put_message(&self, queue: &str, body: String, visibility_timeout_secs: i64, ttl_secs: Option<i64>) -> CoreResult<MessageView> {
        let state = self.queues.get(queue).ok_or_else(|| CoreError::QueueNotFound(queue.to_string()))?;
        let now = Utc::now();
        let ttl = ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
        let expiration_time = if ttl < 0 {
            now + Duration::days(365 * 100)
        } else {
            now + Duration::seconds(ttl)
        };
        let message = Message {
            id: Uuid::new_v4().to_string(),
            body,
            insertion_time: now,
            expiration_time,
            next_visible_time: now + Duration::seconds(visibility_timeout_secs.max(0)),
            dequeue_count: 0,
            pop_receipt: None,
        };
        let view = message.to_view(None, None);
        state.messages.lock().unwrap().push(message);
        Ok(view)
    }

    /// Dequeues up to `count` visible messages, hiding each for `visibility_timeout_secs`
    /// and returning a fresh `pop_receipt` for each one (required to `delete_message`/
    /// `update_message` it afterward). Also lazily evicts any expired messages.
    pub fn get_messages(&self, queue: &str, count: usize, visibility_timeout_secs: i64) -> CoreResult<Vec<MessageView>> {
        let state = self.queues.get(queue).ok_or_else(|| CoreError::QueueNotFound(queue.to_string()))?;
        let now = Utc::now();
        let mut messages = state.messages.lock().unwrap();
        messages.retain(|m| m.expiration_time > now);

        let mut out = Vec::with_capacity(count);
        for m in messages.iter_mut() {
            if out.len() >= count {
                break;
            }
            if m.next_visible_time > now {
                continue;
            }
            m.dequeue_count += 1;
            let pop_receipt = Uuid::new_v4().to_string();
            m.pop_receipt = Some(pop_receipt.clone());
            m.next_visible_time = now + Duration::seconds(visibility_timeout_secs.max(0));
            out.push(m.to_view(Some(pop_receipt), Some(m.next_visible_time)));
        }
        Ok(out)
    }

    /// Returns up to `count` visible messages without changing their visibility/dequeue
    /// count - matches real Azure Queue Storage's `Peek Messages` semantics (no
    /// `pop_receipt`, since nothing was leased).
    pub fn peek_messages(&self, queue: &str, count: usize) -> CoreResult<Vec<MessageView>> {
        let state = self.queues.get(queue).ok_or_else(|| CoreError::QueueNotFound(queue.to_string()))?;
        let now = Utc::now();
        let messages = state.messages.lock().unwrap();
        Ok(messages
            .iter()
            .filter(|m| m.next_visible_time <= now && m.expiration_time > now)
            .take(count)
            .map(|m| m.to_view(None, None))
            .collect())
    }

    pub fn delete_message(&self, queue: &str, message_id: &str, pop_receipt: &str) -> CoreResult<()> {
        let state = self.queues.get(queue).ok_or_else(|| CoreError::QueueNotFound(queue.to_string()))?;
        let mut messages = state.messages.lock().unwrap();
        let idx = messages
            .iter()
            .position(|m| m.id == message_id)
            .ok_or_else(|| CoreError::MessageNotFound(message_id.to_string()))?;
        if messages[idx].pop_receipt.as_deref() != Some(pop_receipt) {
            return Err(CoreError::PopReceiptMismatch(message_id.to_string()));
        }
        messages.remove(idx);
        Ok(())
    }

    /// Updates a leased message's visibility timeout (and optionally its body), returning a
    /// fresh `pop_receipt` - matches real Azure Queue Storage's `Update Message`, used to
    /// extend processing time or requeue-with-new-content without a full delete+re-put.
    pub fn update_message(
        &self,
        queue: &str,
        message_id: &str,
        pop_receipt: &str,
        visibility_timeout_secs: i64,
        body: Option<String>,
    ) -> CoreResult<MessageView> {
        let state = self.queues.get(queue).ok_or_else(|| CoreError::QueueNotFound(queue.to_string()))?;
        let now = Utc::now();
        let mut messages = state.messages.lock().unwrap();
        let m = messages
            .iter_mut()
            .find(|m| m.id == message_id)
            .ok_or_else(|| CoreError::MessageNotFound(message_id.to_string()))?;
        if m.pop_receipt.as_deref() != Some(pop_receipt) {
            return Err(CoreError::PopReceiptMismatch(message_id.to_string()));
        }
        if let Some(body) = body {
            m.body = body;
        }
        m.next_visible_time = now + Duration::seconds(visibility_timeout_secs.max(0));
        let new_pop_receipt = Uuid::new_v4().to_string();
        m.pop_receipt = Some(new_pop_receipt.clone());
        Ok(m.to_view(Some(new_pop_receipt), Some(m.next_visible_time)))
    }

    pub fn clear_messages(&self, queue: &str) -> CoreResult<()> {
        let state = self.queues.get(queue).ok_or_else(|| CoreError::QueueNotFound(queue.to_string()))?;
        state.messages.lock().unwrap().clear();
        Ok(())
    }

    // ------------------------------------------------------------ persistence

    /// Captures the whole store as a serializable [`QueueStoreDump`].
    pub fn dump(&self) -> QueueStoreDump {
        let mut queues: Vec<QueueDump> = self
            .queues
            .iter()
            .map(|entry| {
                let name = entry.key().clone();
                let state = entry.value();
                let messages = state
                    .messages
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|m| MessageDump {
                        id: m.id.clone(),
                        body: m.body.clone(),
                        insertion_time: m.insertion_time,
                        expiration_time: m.expiration_time,
                        next_visible_time: m.next_visible_time,
                        dequeue_count: m.dequeue_count,
                    })
                    .collect();
                QueueDump {
                    name,
                    created_at: state.created_at,
                    messages,
                }
            })
            .collect();
        queues.sort_by(|a, b| a.name.cmp(&b.name));
        QueueStoreDump { queues }
    }

    /// Rebuilds a store from a previously-captured [`QueueStoreDump`] (loaded from disk).
    /// Every message comes back unleased (see [`MessageDump`]'s doc comment) regardless of
    /// whether it had an in-flight `pop_receipt` at the time it was dumped.
    pub fn restore(dump: QueueStoreDump) -> Self {
        let store = Self::new();
        for queue in dump.queues {
            let messages = queue
                .messages
                .into_iter()
                .map(|m| Message {
                    id: m.id,
                    body: m.body,
                    insertion_time: m.insertion_time,
                    expiration_time: m.expiration_time,
                    next_visible_time: m.next_visible_time,
                    dequeue_count: m.dequeue_count,
                    pop_receipt: None,
                })
                .collect();
            store.queues.insert(
                queue.name,
                QueueState {
                    created_at: queue.created_at,
                    messages: StdMutex::new(messages),
                },
            );
        }
        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_delete_roundtrip() {
        let store = QueueStore::new();
        store.create_queue("jobs").unwrap();
        store.put_message("jobs", "hello".to_string(), 0, None).unwrap();

        let got = store.get_messages("jobs", 10, 30).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "hello");
        assert_eq!(got[0].dequeue_count, 1);
        let pop_receipt = got[0].pop_receipt.clone().unwrap();

        // Message is now invisible (leased) - a second get shouldn't return it.
        assert_eq!(store.get_messages("jobs", 10, 30).unwrap().len(), 0);

        store.delete_message("jobs", &got[0].message_id, &pop_receipt).unwrap();
        assert_eq!(store.peek_messages("jobs", 10).unwrap().len(), 0);
    }

    #[test]
    fn wrong_pop_receipt_is_rejected() {
        let store = QueueStore::new();
        store.create_queue("jobs").unwrap();
        store.put_message("jobs", "hello".to_string(), 0, None).unwrap();
        let got = store.get_messages("jobs", 1, 30).unwrap();
        let err = store.delete_message("jobs", &got[0].message_id, "wrong-receipt").unwrap_err();
        assert!(matches!(err, CoreError::PopReceiptMismatch(_)));
    }

    #[test]
    fn peek_does_not_affect_dequeue_count() {
        let store = QueueStore::new();
        store.create_queue("jobs").unwrap();
        store.put_message("jobs", "hello".to_string(), 0, None).unwrap();
        let peeked = store.peek_messages("jobs", 10).unwrap();
        assert_eq!(peeked[0].dequeue_count, 0);
        assert!(peeked[0].pop_receipt.is_none());
        // Still visible for a real get afterward.
        assert_eq!(store.get_messages("jobs", 10, 30).unwrap().len(), 1);
    }
}
