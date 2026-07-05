use crate::error::{CoreError, CoreResult};
use crate::model::{
    BrokeredMessage, DeliveryMode, EntityDump, EntityKind, EntityOptions, EntityStats,
    MessageState, NewMessage,
};
use chrono::Utc;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};
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

/// A cheap, cloneable handle to a running entity actor.
#[derive(Clone)]
pub struct EntityHandle {
    name: Arc<String>,
    tx: mpsc::Sender<Command>,
    notify: Arc<Notify>,
}

impl EntityHandle {
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

struct LockEntry {
    sequence_number: i64,
    locked_until: chrono::DateTime<Utc>,
}

struct EntityState {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    kind: EntityKind,
    options: EntityOptions,
    next_sequence: i64,
    active: VecDeque<BrokeredMessage>,
    scheduled: Vec<BrokeredMessage>,
    deferred: HashMap<i64, BrokeredMessage>,
    dead_letter: VecDeque<BrokeredMessage>,
    /// messages currently out on a PeekLock, keyed by lock token
    locks: HashMap<Uuid, LockEntry>,
    /// sequence_number -> message, for messages currently locked (removed from `active`)
    locked_messages: HashMap<i64, BrokeredMessage>,
    /// session ids currently locked by a session receiver
    locked_sessions: std::collections::HashSet<String>,
}

impl EntityState {
    fn new(name: String, kind: EntityKind, options: EntityOptions) -> Self {
        Self {
            name,
            kind,
            options,
            next_sequence: 1,
            active: VecDeque::new(),
            scheduled: Vec::new(),
            deferred: HashMap::new(),
            dead_letter: VecDeque::new(),
            locks: HashMap::new(),
            locked_messages: HashMap::new(),
            locked_sessions: std::collections::HashSet::new(),
        }
    }

    fn stats(&self) -> EntityStats {
        EntityStats {
            active_count: self.active.len() as u64,
            scheduled_count: self.scheduled.len() as u64,
            deferred_count: self.deferred.len() as u64,
            dead_letter_count: self.dead_letter.len() as u64,
        }
    }

    /// Snapshots this entity's durable state for persisting to disk. Any message
    /// currently checked out to a receiver (peek-locked) is folded back into `active` as
    /// if its lock had just expired, since no receiver could still be holding it after a
    /// process restart.
    fn export(&self) -> EntityDump {
        let mut active: Vec<BrokeredMessage> = self.active.iter().cloned().collect();
        for msg in self.locked_messages.values() {
            let mut msg = msg.clone();
            msg.lock_token = None;
            msg.locked_until = None;
            msg.state = MessageState::Active;
            active.push(msg);
        }
        active.sort_by_key(|m| m.sequence_number);

        EntityDump {
            options: self.options.clone(),
            next_sequence: self.next_sequence,
            active,
            scheduled: self.scheduled.clone(),
            deferred: self.deferred.values().cloned().collect(),
            dead_letter: self.dead_letter.iter().cloned().collect(),
        }
    }

    /// Replaces this entity's durable state with a previously-exported snapshot, discarding
    /// any in-progress locks/session locks.
    fn restore(&mut self, dump: EntityDump) {
        self.options = dump.options;
        self.next_sequence = dump.next_sequence;
        self.active = dump.active.into_iter().collect();
        self.scheduled = dump.scheduled;
        self.deferred = dump
            .deferred
            .into_iter()
            .map(|m| (m.sequence_number, m))
            .collect();
        self.dead_letter = dump.dead_letter.into_iter().collect();
        self.locks.clear();
        self.locked_messages.clear();
        self.locked_sessions.clear();
    }

    fn enqueue(&mut self, msg: NewMessage) -> i64 {
        let seq = self.next_sequence;
        self.next_sequence += 1;
        let now = Utc::now();
        let ttl = msg.time_to_live.or(self.options.default_ttl);
        let expires_at = ttl.map(|d| now + d);
        let mut brokered = BrokeredMessage {
            sequence_number: seq,
            message_id: msg.message_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            body: msg.body,
            content_type: msg.content_type,
            correlation_id: msg.correlation_id,
            session_id: msg.session_id,
            partition_key: msg.partition_key,
            properties: msg.properties,
            enqueued_time: now,
            scheduled_enqueue_time: msg.scheduled_enqueue_time,
            expires_at,
            delivery_count: 0,
            state: MessageState::Active,
            lock_token: None,
            locked_until: None,
            dead_letter_reason: None,
            dead_letter_description: None,
        };
        if brokered.scheduled_enqueue_time.filter(|t| *t > now).is_some() {
            brokered.state = MessageState::Scheduled;
            self.scheduled.push(brokered);
        } else {
            self.active.push_back(brokered);
        }
        seq
    }

    /// Move due scheduled messages into active, expire due locks (back to active, bumping
    /// delivery count / dead-lettering if exhausted), and expire TTL'd active messages.
    fn tick(&mut self) {
        let now = Utc::now();

        // promote scheduled -> active
        let mut still_scheduled = Vec::new();
        for msg in self.scheduled.drain(..) {
            if msg.scheduled_enqueue_time.map(|t| t <= now).unwrap_or(true) {
                let mut msg = msg;
                msg.state = MessageState::Active;
                self.active.push_back(msg);
            } else {
                still_scheduled.push(msg);
            }
        }
        self.scheduled = still_scheduled;

        // expire locks
        let expired: Vec<Uuid> = self
            .locks
            .iter()
            .filter(|(_, entry)| entry.locked_until <= now)
            .map(|(token, _)| *token)
            .collect();
        for token in expired {
            if let Some(entry) = self.locks.remove(&token) {
                if let Some(mut msg) = self.locked_messages.remove(&entry.sequence_number) {
                    msg.delivery_count += 1;
                    msg.lock_token = None;
                    msg.locked_until = None;
                    if msg.delivery_count >= self.options.max_delivery_count {
                        msg.state = MessageState::DeadLettered;
                        msg.dead_letter_reason = Some("MaxDeliveryCountExceeded".into());
                        self.dead_letter.push_back(msg);
                    } else {
                        msg.state = MessageState::Active;
                        self.active.push_front(msg);
                    }
                }
            }
        }

        // expire TTL on active messages
        let mut kept = VecDeque::with_capacity(self.active.len());
        for mut msg in self.active.drain(..) {
            if msg.expires_at.map(|t| t <= now).unwrap_or(false) {
                if self.options.dead_letter_on_expiration {
                    msg.state = MessageState::DeadLettered;
                    msg.dead_letter_reason = Some("TTLExpired".into());
                    self.dead_letter.push_back(msg);
                }
                // else: silently dropped, matching Service Bus default behavior
            } else {
                kept.push_back(msg);
            }
        }
        self.active = kept;
    }

    fn try_receive(&mut self, mode: DeliveryMode) -> Option<BrokeredMessage> {
        let mut msg = self.active.pop_front()?;
        self.finish_receive(&mut msg, mode);
        Some(msg)
    }

    /// Accepts a message session. See [`Command::AcceptSession`] for semantics.
    fn accept_session(&mut self, requested: Option<String>) -> Option<String> {
        tracing::info!(
            entity = %self.name,
            ?requested,
            active_count = self.active.len(),
            active_session_ids = ?self.active.iter().map(|m| m.session_id.clone()).collect::<Vec<_>>(),
            locked_sessions = ?self.locked_sessions,
            "accept_session called"
        );
        match requested {
            Some(id) => {
                if self.locked_sessions.contains(&id) {
                    return None;
                }
                self.locked_sessions.insert(id.clone());
                Some(id)
            }
            None => {
                let candidate = self
                    .active
                    .iter()
                    .filter_map(|m| m.session_id.clone())
                    .find(|sid| !self.locked_sessions.contains(sid));
                if let Some(id) = candidate {
                    self.locked_sessions.insert(id.clone());
                    Some(id)
                } else {
                    None
                }
            }
        }
    }

    fn release_session(&mut self, session_id: &str) {
        self.locked_sessions.remove(session_id);
    }

    /// Like `try_receive`, but only considers messages belonging to `session_id`.
    fn try_receive_session(&mut self, session_id: &str, mode: DeliveryMode) -> Option<BrokeredMessage> {
        let idx = self
            .active
            .iter()
            .position(|m| m.session_id.as_deref() == Some(session_id))?;
        let mut msg = self.active.remove(idx)?;
        self.finish_receive(&mut msg, mode);
        Some(msg)
    }

    /// Shared bookkeeping for handing a dequeued message to a receiver, for both plain and
    /// session-scoped receives.
    fn finish_receive(&mut self, msg: &mut BrokeredMessage, mode: DeliveryMode) {
        msg.delivery_count += 1;
        if let DeliveryMode::PeekLock = mode {
            let token = Uuid::new_v4();
            let locked_until = Utc::now() + self.options.lock_duration;
            msg.lock_token = Some(token);
            msg.locked_until = Some(locked_until);
            self.locks.insert(
                token,
                LockEntry {
                    sequence_number: msg.sequence_number,
                    locked_until,
                },
            );
            self.locked_messages.insert(msg.sequence_number, msg.clone());
        }
    }

    fn complete(&mut self, token: Uuid) -> CoreResult<()> {
        let entry = self.locks.remove(&token).ok_or(CoreError::LockLost)?;
        self.locked_messages.remove(&entry.sequence_number);
        Ok(())
    }

    fn abandon(&mut self, token: Uuid) -> CoreResult<()> {
        let entry = self.locks.remove(&token).ok_or(CoreError::LockLost)?;
        if let Some(mut msg) = self.locked_messages.remove(&entry.sequence_number) {
            msg.lock_token = None;
            msg.locked_until = None;
            msg.state = MessageState::Active;
            self.active.push_front(msg);
        }
        Ok(())
    }

    fn dead_letter(
        &mut self,
        token: Uuid,
        reason: Option<String>,
        description: Option<String>,
    ) -> CoreResult<()> {
        let entry = self.locks.remove(&token).ok_or(CoreError::LockLost)?;
        if let Some(mut msg) = self.locked_messages.remove(&entry.sequence_number) {
            msg.lock_token = None;
            msg.locked_until = None;
            msg.state = MessageState::DeadLettered;
            msg.dead_letter_reason = reason;
            msg.dead_letter_description = description;
            self.dead_letter.push_back(msg);
        }
        Ok(())
    }

    fn defer(&mut self, token: Uuid) -> CoreResult<()> {
        let entry = self.locks.remove(&token).ok_or(CoreError::LockLost)?;
        if let Some(mut msg) = self.locked_messages.remove(&entry.sequence_number) {
            msg.lock_token = None;
            msg.locked_until = None;
            msg.state = MessageState::Deferred;
            self.deferred.insert(msg.sequence_number, msg);
        }
        Ok(())
    }

    fn renew_lock(&mut self, token: Uuid) -> CoreResult<chrono::DateTime<Utc>> {
        let new_until = Utc::now() + self.options.lock_duration;
        let entry = self.locks.get_mut(&token).ok_or(CoreError::LockLost)?;
        entry.locked_until = new_until;
        if let Some(msg) = self.locked_messages.get_mut(&entry.sequence_number) {
            msg.locked_until = Some(new_until);
        }
        Ok(new_until)
    }

    fn peek(&self, state: MessageState, from_sequence: i64, max_count: u32) -> Vec<BrokeredMessage> {
        let source: Box<dyn Iterator<Item = &BrokeredMessage>> = match state {
            MessageState::Active => Box::new(self.active.iter()),
            MessageState::Scheduled => Box::new(self.scheduled.iter()),
            MessageState::Deferred => Box::new(self.deferred.values()),
            MessageState::DeadLettered => Box::new(self.dead_letter.iter()),
        };
        let mut msgs: Vec<&BrokeredMessage> = source
            .filter(|m| m.sequence_number >= from_sequence)
            .collect();
        msgs.sort_by_key(|m| m.sequence_number);
        msgs.into_iter()
            .take(max_count as usize)
            .cloned()
            .collect()
    }

    fn purge(&mut self) -> u64 {
        let n = self.active.len() as u64;
        self.active.clear();
        self.locks.clear();
        self.locked_messages.clear();
        n
    }

    /// Removes one message by sequence number from whichever bucket currently holds it
    /// (active, scheduled, deferred, dead-letter, or currently locked out to a receiver).
    /// Returns `true` if a message was found and removed.
    fn delete_message(&mut self, sequence_number: i64) -> bool {
        let before = self.active.len();
        self.active.retain(|m| m.sequence_number != sequence_number);
        if self.active.len() != before {
            return true;
        }

        let before = self.scheduled.len();
        self.scheduled.retain(|m| m.sequence_number != sequence_number);
        if self.scheduled.len() != before {
            return true;
        }

        if self.deferred.remove(&sequence_number).is_some() {
            return true;
        }

        let before = self.dead_letter.len();
        self.dead_letter.retain(|m| m.sequence_number != sequence_number);
        if self.dead_letter.len() != before {
            return true;
        }

        if self.locked_messages.remove(&sequence_number).is_some() {
            let token = self
                .locks
                .iter()
                .find(|(_, entry)| entry.sequence_number == sequence_number)
                .map(|(token, _)| *token);
            if let Some(token) = token {
                self.locks.remove(&token);
            }
            return true;
        }

        false
    }

    /// Removes `sequence_number` from the dead-letter bucket (if present) and re-enqueues
    /// it as a brand new active message (new sequence number, delivery count reset, no
    /// dead-letter reason/description/lock) - same effect as `enqueue`, just sourced from a
    /// dead-lettered message's contents instead of a fresh `NewMessage`.
    fn resubmit_dead_letter(&mut self, sequence_number: i64) -> CoreResult<i64> {
        let pos = self
            .dead_letter
            .iter()
            .position(|m| m.sequence_number == sequence_number)
            .ok_or(CoreError::SequenceNotFound)?;
        let msg = self.dead_letter.remove(pos).expect("position just found");
        let new_msg = NewMessage {
            message_id: Some(msg.message_id),
            body: msg.body,
            content_type: msg.content_type,
            correlation_id: msg.correlation_id,
            session_id: msg.session_id,
            partition_key: msg.partition_key,
            properties: msg.properties,
            scheduled_enqueue_time: None,
            time_to_live: None,
        };
        Ok(self.enqueue(new_msg))
    }
}

pub fn spawn_entity(name: String, kind: EntityKind, options: EntityOptions) -> EntityHandle {
    let (tx, mut rx) = mpsc::channel::<Command>(1024);
    let notify = Arc::new(Notify::new());
    let notify_for_task = notify.clone();
    let handle_name = Arc::new(name.clone());

    tokio::spawn(async move {
        let mut state = EntityState::new(name, kind, options);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let had_active = !state.active.is_empty();
                    state.tick();
                    if !had_active && !state.active.is_empty() {
                        notify_for_task.notify_waiters();
                    }
                }
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        Command::Send { msg, reply } => {
                            let seq = state.enqueue(msg);
                            notify_for_task.notify_waiters();
                            let _ = reply.send(Ok(seq));
                        }
                        Command::TryReceive { mode, reply } => {
                            let msg = state.try_receive(mode);
                            let _ = reply.send(Ok(msg));
                        }
                        Command::AcceptSession { requested, reply } => {
                            let granted = state.accept_session(requested);
                            let _ = reply.send(granted);
                        }
                        Command::ReleaseSession { session_id } => {
                            state.release_session(&session_id);
                        }
                        Command::TryReceiveSession { session_id, mode, reply } => {
                            let msg = state.try_receive_session(&session_id, mode);
                            let _ = reply.send(Ok(msg));
                        }
                        Command::Complete { lock_token, reply } => {
                            let _ = reply.send(state.complete(lock_token));
                        }
                        Command::Abandon { lock_token, reply } => {
                            let res = state.abandon(lock_token);
                            notify_for_task.notify_waiters();
                            let _ = reply.send(res);
                        }
                        Command::DeadLetter { lock_token, reason, description, reply } => {
                            let _ = reply.send(state.dead_letter(lock_token, reason, description));
                        }
                        Command::Defer { lock_token, reply } => {
                            let _ = reply.send(state.defer(lock_token));
                        }
                        Command::RenewLock { lock_token, reply } => {
                            let _ = reply.send(state.renew_lock(lock_token));
                        }
                        Command::Peek { state: st, from_sequence, max_count, reply } => {
                            let _ = reply.send(state.peek(st, from_sequence, max_count));
                        }
                        Command::Purge { reply } => {
                            let _ = reply.send(state.purge());
                        }
                        Command::Delete { sequence_number, reply } => {
                            let _ = reply.send(state.delete_message(sequence_number));
                        }
                        Command::Resubmit { sequence_number, reply } => {
                            let res = state.resubmit_dead_letter(sequence_number);
                            if res.is_ok() {
                                notify_for_task.notify_waiters();
                            }
                            let _ = reply.send(res);
                        }
                        Command::Stats { reply } => {
                            let _ = reply.send(state.stats());
                        }
                        Command::Export { reply } => {
                            let _ = reply.send(state.export());
                        }
                        Command::Restore { dump, reply } => {
                            state.restore(dump);
                            notify_for_task.notify_waiters();
                            let _ = reply.send(());
                        }
                        Command::Tick => {
                            state.tick();
                        }
                    }
                }
            }
        }
    });

    EntityHandle {
        name: handle_name,
        tx,
        notify,
    }
}
