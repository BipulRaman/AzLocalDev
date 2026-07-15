//! [`EntityState`]: the pure, synchronous state machine backing one queue or subscription
//! actor task (see [`super::task::spawn_entity`]) - message buckets (active/scheduled/
//! deferred/dead-letter), peek-lock tracking, and session-lock tracking. Has no async/
//! tokio/[`super::command::Command`] dependency at all; [`super::task`] is what wires this
//! up to an actor loop dispatching `Command`s to these methods.

use crate::error::{CoreError, CoreResult};
use crate::model::{
    BrokeredMessage, DeliveryMode, EntityDump, EntityKind, EntityOptions, EntityStats,
    MessageState, NewMessage,
};
use chrono::Utc;
use std::collections::{HashMap, VecDeque};
use uuid::Uuid;

pub(super) struct LockEntry {
    pub(super) sequence_number: i64,
    pub(super) locked_until: chrono::DateTime<Utc>,
}

pub(super) struct EntityState {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    kind: EntityKind,
    options: EntityOptions,
    next_sequence: i64,
    pub(super) active: VecDeque<BrokeredMessage>,
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
    pub(super) fn new(name: String, kind: EntityKind, options: EntityOptions) -> Self {
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

    pub(super) fn stats(&self) -> EntityStats {
        EntityStats {
            active_count: self.active.len() as u64,
            scheduled_count: self.scheduled.len() as u64,
            deferred_count: self.deferred.len() as u64,
            dead_letter_count: self.dead_letter.len() as u64,
            requires_session: self.options.requires_session,
        }
    }

    /// Snapshots this entity's durable state for persisting to disk. Any message
    /// currently checked out to a receiver (peek-locked) is folded back into `active` as
    /// if its lock had just expired, since no receiver could still be holding it after a
    /// process restart.
    pub(super) fn export(&self) -> EntityDump {
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
    pub(super) fn restore(&mut self, dump: EntityDump) {
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

    pub(super) fn enqueue(&mut self, msg: NewMessage) -> CoreResult<i64> {
        // A session-required entity rejects any message that doesn't carry a session id,
        // mirroring real Service Bus (which throws InvalidOperationException on send).
        if self.options.requires_session
            && msg.session_id.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            return Err(CoreError::SessionRequired);
        }
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
        Ok(seq)
    }

    /// Move due scheduled messages into active, expire due locks (back to active, bumping
    /// delivery count / dead-lettering if exhausted), and expire TTL'd active messages.
    pub(super) fn tick(&mut self) {
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

    pub(super) fn try_receive(&mut self, mode: DeliveryMode) -> Option<BrokeredMessage> {
        let mut msg = self.active.pop_front()?;
        self.finish_receive(&mut msg, mode);
        Some(msg)
    }

    /// Accepts a message session. See [`super::command::Command::AcceptSession`] for semantics.
    pub(super) fn accept_session(&mut self, requested: Option<String>) -> Option<String> {
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

    pub(super) fn release_session(&mut self, session_id: &str) {
        self.locked_sessions.remove(session_id);
    }

    /// Like `try_receive`, but only considers messages belonging to `session_id`.
    pub(super) fn try_receive_session(&mut self, session_id: &str, mode: DeliveryMode) -> Option<BrokeredMessage> {
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

    pub(super) fn complete(&mut self, token: Uuid) -> CoreResult<()> {
        let entry = self.locks.remove(&token).ok_or(CoreError::LockLost)?;
        self.locked_messages.remove(&entry.sequence_number);
        Ok(())
    }

    pub(super) fn abandon(&mut self, token: Uuid) -> CoreResult<()> {
        let entry = self.locks.remove(&token).ok_or(CoreError::LockLost)?;
        if let Some(mut msg) = self.locked_messages.remove(&entry.sequence_number) {
            msg.lock_token = None;
            msg.locked_until = None;
            msg.state = MessageState::Active;
            self.active.push_front(msg);
        }
        Ok(())
    }

    pub(super) fn dead_letter(
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

    pub(super) fn defer(&mut self, token: Uuid) -> CoreResult<()> {
        let entry = self.locks.remove(&token).ok_or(CoreError::LockLost)?;
        if let Some(mut msg) = self.locked_messages.remove(&entry.sequence_number) {
            msg.lock_token = None;
            msg.locked_until = None;
            msg.state = MessageState::Deferred;
            self.deferred.insert(msg.sequence_number, msg);
        }
        Ok(())
    }

    pub(super) fn renew_lock(&mut self, token: Uuid) -> CoreResult<chrono::DateTime<Utc>> {
        let new_until = Utc::now() + self.options.lock_duration;
        let entry = self.locks.get_mut(&token).ok_or(CoreError::LockLost)?;
        entry.locked_until = new_until;
        if let Some(msg) = self.locked_messages.get_mut(&entry.sequence_number) {
            msg.locked_until = Some(new_until);
        }
        Ok(new_until)
    }

    pub(super) fn peek(&self, state: MessageState, from_sequence: i64, max_count: u32) -> Vec<BrokeredMessage> {
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

    pub(super) fn purge(&mut self) -> u64 {
        let n = self.active.len() as u64;
        self.active.clear();
        self.locks.clear();
        self.locked_messages.clear();
        n
    }

    /// Removes one message by sequence number from whichever bucket currently holds it
    /// (active, scheduled, deferred, dead-letter, or currently locked out to a receiver).
    /// Returns `true` if a message was found and removed.
    pub(super) fn delete_message(&mut self, sequence_number: i64) -> bool {
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
    pub(super) fn resubmit_dead_letter(&mut self, sequence_number: i64) -> CoreResult<i64> {
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
        self.enqueue(new_msg)
    }
}
