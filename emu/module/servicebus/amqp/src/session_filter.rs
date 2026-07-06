//! Service Bus message-session filter/lock resolution, resolved while pre-processing an
//! incoming Attach - before it's handed to the `LinkAcceptor` - so a session receiver's
//! granted session id can be rewritten into the Attach response's `Source` filter (see
//! [`set_session_filter_value`]'s doc comment for why that has to happen at this exact point).

use std::time::Duration;

use fe2o3_amqp::types::definitions::Role;
use fe2o3_amqp::types::messaging::Source;
use fe2o3_amqp::types::performatives::Attach;
use fe2o3_amqp::types::primitives::{Symbol, Value};

use emu_servicebus_core::{Broker, EntityHandle};

use crate::links::resolve_entity;

/// The AMQP filter descriptor Service Bus session receivers attach with. Its value is either
/// a specific session id (client wants that exact session) or null (client wants "whichever
/// session is next available").
const SESSION_FILTER_NAME: &str = "com.microsoft:session-filter";

/// Returns `Some(requested)` if `source` carries a session filter (`requested` being `None`
/// for "next available session" or `Some(id)` for a specific one), or `None` if `source`
/// doesn't request session semantics at all (a plain, non-session receiver).
fn requested_session(source: &Source) -> Option<Option<String>> {
    let filter = source.filter.as_ref()?;
    let (_, value) = filter
        .iter()
        .find(|(k, _)| *k == &Symbol::from(SESSION_FILTER_NAME))?;
    Some(session_value_to_string(value))
}

fn session_value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Symbol(s) => Some(s.to_string()),
        Value::Described(described) => session_value_to_string(&described.value),
        _ => None,
    }
}

/// Outcome of trying to grant a message session while pre-processing an incoming Attach, so
/// the downstream link handler knows whether (and which) session it's scoped to without
/// re-deriving it (and accidentally trying to re-lock the same session a second time).
pub(crate) enum SessionOutcome {
    /// A session (specific or "next available") was locked for this link.
    Granted(String),
    /// The client wanted a session but none could be granted in time.
    Requested,
}

/// Overwrites the `com.microsoft:session-filter` entry's value (in place, before the Attach is
/// accepted) with the concrete session id we granted. This is what lets a client that asked for
/// "next available session" learn which session it actually got. The Azure SDK reads this back
/// via `FilterSet.TryGetValue<string>(...)` (see `AmqpReceiver.OpenReceiverLinkAsync`), which
/// requires a plain AMQP string - NOT a described/wrapped value, which decodes to `null` there
/// and makes the client abort the link with "Failed to retreive session ID from broker."
fn set_session_filter_value(source: &mut Source, session_id: &str) {
    let Some(filter) = source.filter.as_mut() else { return };
    if let Some(existing) = filter.get_mut(&Symbol::from(SESSION_FILTER_NAME)) {
        *existing = Value::String(session_id.to_string());
    }
}

/// How long a receiver link asking for a message session (a specific id, or "next available")
/// keeps polling before giving up and detaching. This runs in its own spawned task (see the call
/// site in `crate::connection::handle_connection`), so a slow poll only delays completion of
/// THIS ONE link's attach, and no longer blocks any other concurrent link's attach on the same
/// AMQP session. Kept generous (10 minutes) because a short bound here (previously 5s, run
/// inline and blocking) made `ServiceBusSessionProcessor`'s idle concurrent "next available
/// session" listener slots detach/reattach in a tight loop, spamming `Failed to retreive session
/// ID from broker (GeneralError)` client-side every few seconds even though nothing was
/// actually wrong; there just wasn't a session to hand out yet.
const SESSION_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SESSION_POLL_MAX_ATTEMPTS: u32 = 2_400; // 2_400 * 250ms = 10 minutes

/// Pre-processes an incoming Attach *before* it is handed to the `LinkAcceptor`: fills in a
/// missing `initial-delivery-count` (fe2o3-amqp otherwise hard-rejects such attaches), and, for
/// Service Bus session-filter receivers, polls the broker for a session lock and rewrites the
/// `Source` filter with the concrete granted id. Deliberately takes no `session`/`link_acceptor`
/// (unlike the single-call helper this replaces) so it can run in its own spawned task without
/// holding the exclusive `ListenerSessionHandle` borrow the caller needs to keep discovering/
/// finalizing OTHER concurrent attaches on the same AMQP session while this one is still polling.
pub(crate) async fn resolve_attach_session(
    mut remote_attach: Attach,
    broker: &Broker,
) -> (Attach, Option<SessionOutcome>, Option<(EntityHandle, String)>) {
    let has_filter = remote_attach
        .source
        .as_ref()
        .and_then(|s| s.filter.as_ref())
        .map(|f| f.keys().map(|k| k.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    tracing::info!(
        role = ?remote_attach.role,
        name = %remote_attach.name,
        source_address = ?remote_attach.source.as_ref().and_then(|s| s.address.clone()),
        has_target = remote_attach.target.is_some(),
        dynamic = remote_attach.source.as_ref().map(|s| s.dynamic).unwrap_or(false),
        initial_delivery_count = ?remote_attach.initial_delivery_count,
        filter_keys = ?has_filter,
        "incoming AMQP attach"
    );

    // fe2o3-amqp hard-rejects (LocalReceiver(InitialDeliveryCountIsNone)) any attach where the
    // remote acts as sender but omits `initial-delivery-count`, with no builder option to relax
    // it. Real clients (e.g. the Azure SDK's management/session-RPC "duplex" links) sometimes
    // omit it since it's only meaningful once the sender actually starts transferring, so we
    // default it to 0 here - purely a bookkeeping value, not a real message count.
    if remote_attach.role == Role::Sender && remote_attach.initial_delivery_count.is_none() {
        remote_attach.initial_delivery_count = Some(0);
    }

    let mut outcome = None;
    // If we grant a session lock below, we must remember which entity granted it so that,
    // if `accept_incoming_attach` subsequently fails (e.g. the client races our polling with
    // its own timeout/cancellation and detaches), we release the lock instead of leaking it
    // forever - a leaked lock would permanently block every future receiver, including on
    // brand new connections, since the lock lives in the broker, not on this link/connection.
    let mut granted_entity = None;
    if remote_attach.role == Role::Receiver {
        if let Some(source) = remote_attach.source.as_mut() {
            if !source.dynamic {
                if let Some(requested) = requested_session(source) {
                    let address = source.address.clone().unwrap_or_default();
                    tracing::info!(%address, ?requested, "session filter detected, resolving entity");
                    if let Some(entity) = resolve_entity(broker, &address) {
                        let mut granted = None;
                        for attempt in 0..SESSION_POLL_MAX_ATTEMPTS {
                            if let Some(id) = entity.accept_session(requested.clone()).await {
                                tracing::info!(%address, session_id = %id, attempt, "session granted");
                                granted = Some(id);
                                break;
                            }
                            tokio::time::sleep(SESSION_POLL_INTERVAL).await;
                        }
                        outcome = Some(match granted {
                            Some(id) => {
                                set_session_filter_value(source, &id);
                                granted_entity = Some((entity, id.clone()));
                                SessionOutcome::Granted(id)
                            }
                            None => {
                                tracing::info!(%address, "no session could be granted after retries");
                                SessionOutcome::Requested
                            }
                        });
                    } else {
                        tracing::warn!(%address, "session filter present but entity could not be resolved");
                    }
                }
            }
        }
    }

    (remote_attach, outcome, granted_entity)
}
