//! Core domain model for the Service Bus emulator.
//!
//! This crate has no networking / protocol code in it at all — it is driven purely by
//! [`Command`] messages sent to per-entity actor tasks. Transport adapters (AMQP, HTTP
//! management API, UI API) live in separate crates and only ever talk to the [`Broker`].

mod actor;
mod error;
mod model;

pub use actor::{Command, EntityHandle, ReceivedMessage};
pub use error::{CoreError, CoreResult};
pub use model::{
    BrokeredMessage, DeliveryMode, EntityDump, EntityKind, EntityOptions, EntityStats,
    MessageState, NewMessage,
};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Central registry of every queue / subscription actor in the emulator.
///
/// Topics themselves don't hold messages (their subscriptions do); the broker keeps a
/// separate map of topic name -> subscription names purely for routing fan-out on send.
#[derive(Default, Clone)]
pub struct Broker {
    inner: Arc<BrokerInner>,
}

#[derive(Default)]
struct BrokerInner {
    /// queue name -> actor handle
    queues: DashMap<String, EntityHandle>,
    /// topic name -> set of subscription names
    topics: DashMap<String, Vec<String>>,
    /// "topic/subscription" -> actor handle
    subscriptions: DashMap<String, EntityHandle>,
}

impl Broker {
    pub fn new() -> Self {
        Self::default()
    }

    // ---------------------------------------------------------------- queues

    pub fn create_queue(&self, name: &str, options: EntityOptions) -> EntityHandle {
        if let Some(existing) = self.inner.queues.get(name) {
            return existing.clone();
        }
        let handle = actor::spawn_entity(name.to_string(), EntityKind::Queue, options);
        self.inner.queues.insert(name.to_string(), handle.clone());
        handle
    }

    pub fn get_queue(&self, name: &str) -> Option<EntityHandle> {
        self.inner.queues.get(name).map(|e| e.clone())
    }

    pub fn delete_queue(&self, name: &str) -> bool {
        self.inner.queues.remove(name).is_some()
    }

    pub fn list_queues(&self) -> Vec<String> {
        self.inner.queues.iter().map(|e| e.key().clone()).collect()
    }

    // ---------------------------------------------------------------- topics

    pub fn create_topic(&self, name: &str) {
        self.inner.topics.entry(name.to_string()).or_default();
    }

    pub fn list_topics(&self) -> Vec<String> {
        self.inner.topics.iter().map(|e| e.key().clone()).collect()
    }

    pub fn delete_topic(&self, name: &str) -> bool {
        if let Some((_, subs)) = self.inner.topics.remove(name) {
            for sub in subs {
                self.inner.subscriptions.remove(&format!("{name}/{sub}"));
            }
            true
        } else {
            false
        }
    }

    pub fn create_subscription(
        &self,
        topic: &str,
        sub: &str,
        options: EntityOptions,
    ) -> Option<EntityHandle> {
        if !self.inner.topics.contains_key(topic) {
            return None;
        }
        let key = format!("{topic}/{sub}");
        if let Some(existing) = self.inner.subscriptions.get(&key) {
            return Some(existing.clone());
        }
        let handle = actor::spawn_entity(key.clone(), EntityKind::Subscription, options);
        self.inner.subscriptions.insert(key, handle.clone());
        self.inner
            .topics
            .get_mut(topic)
            .unwrap()
            .push(sub.to_string());
        Some(handle)
    }

    pub fn get_subscription(&self, topic: &str, sub: &str) -> Option<EntityHandle> {
        self.inner
            .subscriptions
            .get(&format!("{topic}/{sub}"))
            .map(|e| e.clone())
    }

    pub fn list_subscriptions(&self, topic: &str) -> Vec<String> {
        self.inner
            .topics
            .get(topic)
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Fan out a message sent to a topic to every subscription currently attached to it.
    /// (Rule/filter evaluation happens per-subscription inside the actor in a later phase;
    /// for now every subscription gets a copy of every message.)
    pub async fn publish_to_topic(
        &self,
        topic: &str,
        msg: NewMessage,
    ) -> CoreResult<Vec<i64>> {
        let subs = self
            .inner
            .topics
            .get(topic)
            .ok_or(CoreError::EntityNotFound)?
            .clone();
        let mut seqs = Vec::with_capacity(subs.len());
        for sub in subs {
            if let Some(handle) = self.get_subscription(topic, &sub) {
                seqs.push(handle.send_message(msg.clone()).await?);
            }
        }
        Ok(seqs)
    }

    // ------------------------------------------------------------ persistence

    /// Snapshots every queue, topic, and subscription (with all of their messages) into a
    /// serializable [`BrokerDump`], so callers can persist it to disk (e.g. as JSON) and
    /// restore it via [`Broker::import`] on the next startup.
    pub async fn export(&self) -> BrokerDump {
        let mut queues = Vec::new();
        for entry in self.inner.queues.iter() {
            if let Ok(entity) = entry.value().export().await {
                queues.push(QueueDump {
                    name: entry.key().clone(),
                    entity,
                });
            }
        }

        let mut topics = Vec::new();
        for entry in self.inner.topics.iter() {
            let topic_name = entry.key().clone();
            let mut subscriptions = Vec::new();
            for sub_name in entry.value() {
                if let Some(handle) = self.get_subscription(&topic_name, sub_name) {
                    if let Ok(entity) = handle.export().await {
                        subscriptions.push(SubscriptionDump {
                            name: sub_name.clone(),
                            entity,
                        });
                    }
                }
            }
            topics.push(TopicDump {
                name: topic_name,
                subscriptions,
            });
        }

        BrokerDump { queues, topics }
    }

    /// Recreates every queue, topic, and subscription from a previously-exported
    /// [`BrokerDump`], restoring their options and messages. Intended to be called once,
    /// right after constructing a fresh, empty [`Broker`].
    pub async fn import(&self, dump: BrokerDump) {
        for queue in dump.queues {
            let handle = self.create_queue(&queue.name, queue.entity.options.clone());
            let _ = handle.restore(queue.entity).await;
        }
        for topic in dump.topics {
            self.create_topic(&topic.name);
            for sub in topic.subscriptions {
                if let Some(handle) =
                    self.create_subscription(&topic.name, &sub.name, sub.entity.options.clone())
                {
                    let _ = handle.restore(sub.entity).await;
                }
            }
        }
    }
}

/// A persisted queue: its name plus its full durable state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDump {
    pub name: String,
    pub entity: EntityDump,
}

/// A persisted subscription (scoped to whichever topic it's nested under in
/// [`TopicDump`]): its name plus its full durable state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionDump {
    pub name: String,
    pub entity: EntityDump,
}

/// A persisted topic: its name plus every one of its subscriptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicDump {
    pub name: String,
    pub subscriptions: Vec<SubscriptionDump>,
}

/// A full, serializable snapshot of every queue, topic, and subscription a [`Broker`]
/// holds, produced by [`Broker::export`] and consumed by [`Broker::import`]. This is the
/// unit of on-disk persistence for a single Service Bus emulator instance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrokerDump {
    pub queues: Vec<QueueDump>,
    pub topics: Vec<TopicDump>,
}
