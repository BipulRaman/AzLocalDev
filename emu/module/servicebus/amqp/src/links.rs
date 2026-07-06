//! Per-link message pumping, once an Attach has been resolved to a broker entity: a remote
//! sender pushes messages TO an entity (we accepted it as a [`Receiver`]), a remote receiver
//! pulls messages FROM an entity (we accepted it as a [`Sender`]).

use std::time::Duration;

use fe2o3_amqp::link::receiver::Receiver;
use fe2o3_amqp::link::sender::Sender;
use fe2o3_amqp::types::messaging::{Body, Modified, Outcome};
use fe2o3_amqp::types::primitives::Value;

use emu_servicebus_core::{Broker, DeliveryMode, EntityHandle};

use crate::convert::{amqp_body_to_new_message, new_message_to_amqp};
use crate::session_filter::SessionOutcome;

/// Resolves an AMQP node address (as set on the link's source/target) to a broker entity.
/// Supports plain queue names ("orders") and Service-Bus-style subscription addresses
/// ("events/Subscriptions/audit-log").
pub(crate) fn resolve_entity(broker: &Broker, address: &str) -> Option<EntityHandle> {
    let address = address.trim_matches('/');
    if let Some((topic, rest)) = address.split_once('/') {
        let rest = rest.trim_start_matches("Subscriptions/").trim_start_matches("subscriptions/");
        return broker.get_subscription(topic, rest);
    }
    broker.get_queue(address)
}

/// A remote sender attached to us: they push messages TO an entity. We accepted this as a
/// [`Receiver`] link endpoint.
pub(crate) async fn handle_incoming_sender_link(broker: Broker, mut receiver: Receiver) {
    let address = receiver
        .target()
        .as_ref()
        .and_then(|t| t.address.clone())
        .unwrap_or_default();
    let Some(entity) = resolve_entity(&broker, &address) else {
        tracing::warn!(%address, "sender link attached to unknown entity, detaching");
        let _ = receiver.close().await;
        return;
    };

    loop {
        let delivery = match receiver.recv::<Body<Value>>().await {
            Ok(d) => d,
            Err(err) => {
                tracing::debug!(?err, %address, "receiver link closed");
                break;
            }
        };

        let msg = amqp_body_to_new_message(delivery.message());
        match entity.send_message(msg).await {
            Ok(_seq) => {
                if let Err(err) = receiver.accept(&delivery).await {
                    tracing::debug!(?err, "failed to accept delivery");
                }
            }
            Err(err) => {
                tracing::warn!(?err, "failed to enqueue message from AMQP sender");
                let _ = receiver.reject(&delivery, None).await;
            }
        }
    }
}

/// A remote receiver attached to us: they want to pull messages FROM an entity. We
/// accepted this as a [`Sender`] link endpoint.
pub(crate) async fn handle_incoming_receiver_link(broker: Broker, mut sender: Sender, session_outcome: Option<SessionOutcome>) {
    let address = sender
        .source()
        .as_ref()
        .and_then(|s| s.address.clone())
        .unwrap_or_default();
    tracing::info!(%address, granted = ?session_outcome.as_ref().map(|o| match o {
        SessionOutcome::Granted(id) => id.clone(),
        SessionOutcome::Requested => "<none>".to_string(),
    }), "handling receiver link");
    let Some(entity) = resolve_entity(&broker, &address) else {
        tracing::warn!(%address, "receiver link attached to unknown entity, detaching");
        let _ = sender.close().await;
        return;
    };

    // The session (if any) was already resolved/locked while pre-processing the incoming
    // Attach (see `crate::session_filter::resolve_attach_session`), so we just act on that
    // decision here instead of trying to lock a session a second time.
    let session_id = match session_outcome {
        Some(SessionOutcome::Granted(id)) => Some(id),
        Some(SessionOutcome::Requested) => {
            tracing::info!(%address, "no message session currently available, detaching");
            let _ = sender.close().await;
            return;
        }
        None => None,
    };

    loop {
        let received = match &session_id {
            Some(sid) => {
                entity
                    .receive_session(sid, DeliveryMode::PeekLock, Duration::from_secs(60))
                    .await
            }
            None => entity.receive(DeliveryMode::PeekLock, Duration::from_secs(60)).await,
        };
        let msg = match received {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!(?err, %address, "entity gone, closing sender link");
                break;
            }
        };

        let amqp_message = new_message_to_amqp(&msg);
        let lock_token = msg.lock_token;

        // Azure SDK clients derive a message's lock token directly from the AMQP
        // delivery-tag bytes (`GuidUtilities.TryParseGuidBytes(amqpMessage.DeliveryTag, ...)`
        // in azure-sdk-for-net), requiring exactly 16 bytes to parse as a GUID. fe2o3-amqp's
        // public API only ever auto-generates a 4-byte tag with no override, so without our
        // locally patched `delivery_tag` override on `Sendable` the client would always see
        // an empty lock token and be unable to complete/abandon/dead-letter the message.
        let sendable = match lock_token {
            Some(token) => fe2o3_amqp::Sendable::builder()
                .message(amqp_message)
                .delivery_tag(token.as_bytes().to_vec())
                .build(),
            None => fe2o3_amqp::Sendable::builder().message(amqp_message).build(),
        };

        tracing::info!(%address, sequence_number = msg.sequence_number, "sending message to client");
        let outcome = match tokio::time::timeout(Duration::from_secs(30), sender.send(sendable)).await {
            Ok(Ok(o)) => o,
            Ok(Err(err)) => {
                tracing::warn!(?err, %address, "sender link closed while sending");
                break;
            }
            Err(_) => {
                tracing::warn!(%address, sequence_number = msg.sequence_number, "timed out waiting for client to acknowledge sent message");
                break;
            }
        };
        tracing::info!(%address, sequence_number = msg.sequence_number, ?outcome, "message send completed");

        if let Some(token) = lock_token {
            apply_outcome(&entity, token, outcome).await;
        }
    }

    if let Some(sid) = session_id {
        entity.release_session(sid).await;
    }
}

async fn apply_outcome(entity: &EntityHandle, lock_token: uuid::Uuid, outcome: Outcome) {
    let result = match outcome {
        Outcome::Accepted(_) => entity.complete(lock_token).await,
        Outcome::Released(_) => entity.abandon(lock_token).await,
        Outcome::Rejected(rejected) => {
            let description = rejected.error.as_ref().map(|e| e.description.clone().unwrap_or_default());
            entity
                .dead_letter(lock_token, Some("Rejected".to_string()), description)
                .await
        }
        Outcome::Modified(Modified {
            delivery_failed: Some(true),
            ..
        }) => entity.abandon(lock_token).await,
        Outcome::Modified(_) => entity.abandon(lock_token).await,
    };
    if let Err(err) = result {
        tracing::debug!(?err, "failed to apply delivery outcome to broker entity");
    }
}
