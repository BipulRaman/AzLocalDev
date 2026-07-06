//! [`EntityHandle`]: a cheap, cloneable handle to a running entity actor task (see
//! [`super::task::spawn_entity`]) - the public API every caller (AMQP protocol adapter,
//! HTTP management API, dashboard UI API) uses to talk to a queue or subscription.

use crate::error::{CoreError, CoreResult};
use crate::model::{BrokeredMessage, DeliveryMode, EntityDump, EntityStats, MessageState, NewMessage};
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};
use uuid::Uuid;

use super::command::Command;

/// A cheap, cloneable handle to a running entity actor.
#[derive(Clone)]
pub struct EntityHandle {
    name: Arc<String>,
    tx: mpsc::Sender<Command>,
    notify: Arc<Notify>,
}

impl EntityHandle {
    /// Constructs a handle around an already-spawned actor task's channel/notify handles.
    /// Only [`super::task::spawn_entity`] should ever call this.
    pub(super) fn new(name: Arc<String>, tx: mpsc::Sender<Command>, notify: Arc<Notify>) -> Self {
        Self { name, tx, notify }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> CoreResult<T> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .await
            .map_err(|_| CoreError::ActorGone)?;
        rx.await.map_err(|_| CoreError::ActorGone)
    }

    pub async fn send_message(&self, msg: NewMessage) -> CoreResult<i64> {
        self.call(|reply| Command::Send { msg, reply }).await?
    }

    /// Non-blocking: returns `Ok(None)` immediately if nothing is available right now.
    pub async fn try_receive(
        &self,
        mode: DeliveryMode,
    ) -> CoreResult<Option<BrokeredMessage>> {
        self.call(|reply| Command::TryReceive { mode, reply })
            .await?
    }

    /// Waits (up to `max_wait`) for a message to become available, then receives it.
    /// This is what the AMQP receiver-link loop and any long-polling HTTP endpoint use.
    pub async fn receive(
        &self,
        mode: DeliveryMode,
        max_wait: std::time::Duration,
    ) -> CoreResult<Option<BrokeredMessage>> {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            if let Some(msg) = self.try_receive(mode).await? {
                return Ok(Some(msg));
            }
            let notified = self.notify.notified();
            tokio::select! {
                _ = notified => continue,
                _ = tokio::time::sleep_until(deadline) => return Ok(None),
            }
        }
    }

    /// Tries to lock a message session. See [`Command::AcceptSession`] for semantics.
    pub async fn accept_session(&self, requested: Option<String>) -> Option<String> {
        self.call(|reply| Command::AcceptSession { requested, reply })
            .await
            .ok()
            .flatten()
    }

    /// Releases a session lock previously granted by [`EntityHandle::accept_session`].
    pub async fn release_session(&self, session_id: impl Into<String>) {
        let _ = self
            .tx
            .send(Command::ReleaseSession {
                session_id: session_id.into(),
            })
            .await;
    }

    /// Non-blocking: returns `Ok(None)` immediately if no message for `session_id` is
    /// available right now.
    pub async fn try_receive_session(
        &self,
        session_id: String,
        mode: DeliveryMode,
    ) -> CoreResult<Option<BrokeredMessage>> {
        self.call(|reply| Command::TryReceiveSession {
            session_id,
            mode,
            reply,
        })
        .await?
    }

    /// Session-scoped equivalent of [`EntityHandle::receive`]: waits (up to `max_wait`) for
    /// a message belonging to `session_id`.
    pub async fn receive_session(
        &self,
        session_id: &str,
        mode: DeliveryMode,
        max_wait: std::time::Duration,
    ) -> CoreResult<Option<BrokeredMessage>> {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            if let Some(msg) = self.try_receive_session(session_id.to_string(), mode).await? {
                return Ok(Some(msg));
            }
            let notified = self.notify.notified();
            tokio::select! {
                _ = notified => continue,
                _ = tokio::time::sleep_until(deadline) => return Ok(None),
            }
        }
    }

    pub async fn complete(&self, lock_token: Uuid) -> CoreResult<()> {
        self.call(|reply| Command::Complete { lock_token, reply })
            .await?
    }

    pub async fn abandon(&self, lock_token: Uuid) -> CoreResult<()> {
        self.call(|reply| Command::Abandon { lock_token, reply })
            .await?
    }

    pub async fn dead_letter(
        &self,
        lock_token: Uuid,
        reason: Option<String>,
        description: Option<String>,
    ) -> CoreResult<()> {
        self.call(|reply| Command::DeadLetter {
            lock_token,
            reason,
            description,
            reply,
        })
        .await?
    }

    pub async fn defer(&self, lock_token: Uuid) -> CoreResult<()> {
        self.call(|reply| Command::Defer { lock_token, reply })
            .await?
    }

    pub async fn renew_lock(&self, lock_token: Uuid) -> CoreResult<chrono::DateTime<Utc>> {
        self.call(|reply| Command::RenewLock { lock_token, reply })
            .await?
    }

    pub async fn peek(
        &self,
        state: MessageState,
        from_sequence: i64,
        max_count: u32,
    ) -> CoreResult<Vec<BrokeredMessage>> {
        self.call(|reply| Command::Peek {
            state,
            from_sequence,
            max_count,
            reply,
        })
        .await
    }

    pub async fn purge(&self) -> CoreResult<u64> {
        self.call(|reply| Command::Purge { reply }).await
    }

    /// Permanently deletes one message by sequence number, regardless of which bucket
    /// (active/scheduled/deferred/dead-letter, even currently locked) it's currently in.
    pub async fn delete_message(&self, sequence_number: i64) -> CoreResult<bool> {
        self.call(|reply| Command::Delete {
            sequence_number,
            reply,
        })
        .await
    }

    /// Moves a dead-lettered message back into the active queue as a fresh message. See
    /// [`Command::Resubmit`].
    pub async fn resubmit_dead_letter(&self, sequence_number: i64) -> CoreResult<i64> {
        self.call(|reply| Command::Resubmit {
            sequence_number,
            reply,
        })
        .await?
    }

    pub async fn stats(&self) -> CoreResult<EntityStats> {
        self.call(|reply| Command::Stats { reply }).await
    }

    /// Returns a full, serializable snapshot of this entity's durable state, for
    /// persisting to disk.
    pub async fn export(&self) -> CoreResult<EntityDump> {
        self.call(|reply| Command::Export { reply }).await
    }

    /// Replaces this entity's durable state with a previously-exported snapshot.
    pub async fn restore(&self, dump: EntityDump) -> CoreResult<()> {
        self.call(|reply| Command::Restore { dump, reply }).await
    }
}
