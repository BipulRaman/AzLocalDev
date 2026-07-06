//! Per-connection handling: CBS put-token, session acceptance, and link (sender/receiver)
//! routing. Used by both the plain AMQP and AMQPS listeners (see `crate::listener`) -
//! identical once the transport-level handshake (TLS or not) has completed.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use fe2o3_amqp::acceptor::{LinkAcceptor, LinkEndpoint, SessionAcceptor};
use fe2o3_amqp::link::sender::Sender;
use fe2o3_amqp::types::definitions::Fields;
use fe2o3_amqp::types::performatives::Attach;
use fe2o3_amqp::types::primitives::{Symbol, Value};

use emu_servicebus_core::{Broker, EntityHandle};

use crate::cbs::{handle_cbs_requests, is_cbs_address};
use crate::links::{handle_incoming_receiver_link, handle_incoming_sender_link};
use crate::session_filter::{resolve_attach_session, SessionOutcome};

pub(crate) async fn handle_connection(broker: Broker, mut connection: fe2o3_amqp::acceptor::ListenerConnectionHandle) {
    // Shared slot for the CBS response link: the client attaches a request link (put-
    // token, seen by us as a `Receiver`) and a separate response link (seen by us as a
    // `Sender`) on the same session. We stash the response `Sender` here so the request
    // handler can reply on it once both links are attached.
    let cbs_reply_sender: Arc<Mutex<Option<Sender>>> = Arc::new(Mutex::new(None));

    let session_acceptor = SessionAcceptor::new();
    while let Ok(mut session) = session_acceptor.accept(&mut connection).await {
        let broker = broker.clone();
        let cbs_reply_sender = cbs_reply_sender.clone();
        tokio::spawn(async move {
            // Real Azure SDK clients read a `com.microsoft:locked-until-utc` link
            // property off every accepted link (used for session/message lock
            // expiry) via `Properties.TryGetValue<long>(...)` — i.e. it MUST be a
            // plain AMQP `long` holding .NET `DateTime` ticks (100ns units since
            // 0001-01-01), NOT an AMQP `Timestamp` (ms since Unix epoch). Sending the
            // wrong encoding makes TryGetValue silently fail, so the client falls back
            // to DateTime.MinValue and crashes converting it to a DateTimeOffset. We
            // can't set this per-attach (fe2o3-amqp only exposes static, acceptor-wide
            // link properties), so we set a generous fixed-future value once per
            // session.
            const DOTNET_UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000; // DateTime(1970,1,1).Ticks
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let locked_until_ms = now_ms + 10 * 60 * 1000; // +10 minutes
            let locked_until_ticks = DOTNET_UNIX_EPOCH_TICKS + locked_until_ms * 10_000;
            let mut link_properties = Fields::new();
            link_properties.insert(
                Symbol::from("com.microsoft:locked-until-utc"),
                Value::Long(locked_until_ticks),
            );
            let link_acceptor = LinkAcceptor::builder().properties(link_properties).build();

            // Per-link session-acceptance polling (see `resolve_attach_session`) can legitimately
            // take a long time - e.g. a `ServiceBusSessionProcessor` with several concurrent
            // "next available session" listeners and only one actual session to hand out. That
            // polling now runs in its OWN spawned task and reports back via `ready_tx`/`ready_rx`
            // instead of running inline here, so it never blocks this loop from calling
            // `session.next_incoming_attach()` again for OTHER concurrent links on the same AMQP
            // session - only the exclusive `&mut session` borrow needed to actually discover or
            // finalize an attach happens here, both of which are quick.
            let (ready_tx, mut ready_rx) = mpsc::channel::<(Attach, Option<SessionOutcome>, Option<(EntityHandle, String)>)>(64);
            loop {
                tokio::select! {
                    maybe_attach = session.next_incoming_attach() => {
                        let Some(remote_attach) = maybe_attach else {
                            tracing::warn!("link acceptor stopped for session");
                            break;
                        };
                        let broker = broker.clone();
                        let ready_tx = ready_tx.clone();
                        tokio::spawn(async move {
                            let resolved = resolve_attach_session(remote_attach, &broker).await;
                            let _ = ready_tx.send(resolved).await;
                        });
                    }
                    Some((remote_attach, session_outcome, granted_entity)) = ready_rx.recv() => {
                        match link_acceptor.accept_incoming_attach(remote_attach, &mut session).await {
                            Ok(LinkEndpoint::Receiver(receiver)) => {
                                let address = receiver
                                    .target()
                                    .as_ref()
                                    .and_then(|t| t.address.clone())
                                    .unwrap_or_default();
                                if is_cbs_address(&address) {
                                    let cbs_reply_sender = cbs_reply_sender.clone();
                                    tokio::spawn(async move {
                                        handle_cbs_requests(receiver, cbs_reply_sender).await;
                                    });
                                } else {
                                    let broker = broker.clone();
                                    tokio::spawn(async move {
                                        handle_incoming_sender_link(broker, receiver).await;
                                    });
                                }
                            }
                            Ok(LinkEndpoint::Sender(sender)) => {
                                let address = sender
                                    .source()
                                    .as_ref()
                                    .and_then(|s| s.address.clone())
                                    .unwrap_or_default();
                                if is_cbs_address(&address) {
                                    *cbs_reply_sender.lock().await = Some(sender);
                                } else {
                                    let broker = broker.clone();
                                    tokio::spawn(async move {
                                        handle_incoming_receiver_link(broker, sender, session_outcome).await;
                                    });
                                }
                            }
                            Err(err) => {
                                if let Some((entity, id)) = granted_entity {
                                    tracing::warn!(session_id = %id, ?err, "attach failed after session was granted, releasing lock");
                                    entity.release_session(id).await;
                                }
                                tracing::warn!(?err, "link acceptor stopped for session");
                                break;
                            }
                        }
                    }
                }
            }
        });
    }
}
