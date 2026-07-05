//! AMQP 1.0 protocol adapter. Translates AMQP frames <-> [`emu_servicebus_core::Broker`] commands.
//! Contains no business logic beyond that translation.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex};

use fe2o3_amqp::acceptor::sasl_acceptor::{SaslAcceptor, SaslServerFrame};
use fe2o3_amqp::acceptor::{ConnectionAcceptor, LinkAcceptor, LinkEndpoint, SessionAcceptor};
use fe2o3_amqp::link::receiver::Receiver;
use fe2o3_amqp::link::sender::Sender;
use fe2o3_amqp::types::definitions::Role;
use fe2o3_amqp::types::definitions::Fields;
use fe2o3_amqp::types::messaging::{
    ApplicationProperties, Body, Header, Message, MessageId, Modified, Outcome, Properties, Source,
};
use fe2o3_amqp::types::performatives::Attach;
use fe2o3_amqp::types::primitives::{Array, SimpleValue, Symbol, Value};
use fe2o3_amqp::types::sasl::{SaslCode, SaslInit, SaslOutcome, SaslResponse};
use emu_servicebus_core::{Broker, DeliveryMode, EntityHandle, NewMessage};
use tokio::net::TcpListener;

pub use emu_dev_cert::{load_or_generate as load_or_generate_dev_cert, DevCertificate};

fn message_id_to_string(id: &MessageId) -> String {
    format!("{id:?}")
}

/// A dev-emulator SASL acceptor that authenticates every connection unconditionally.
/// Azure Service Bus connection strings always carry a `SharedAccessKeyName`/`SharedAccessKey`
/// pair, so real SDKs (and Azure Functions' Service Bus trigger) always negotiate SASL before
/// opening the AMQP connection. Since this is a local, no-real-security emulator, any
/// credentials are accepted - there is nothing to actually validate them against.
#[derive(Debug, Clone, Default)]
struct AcceptAnyCredentials;

impl SaslAcceptor for AcceptAnyCredentials {
    fn mechanisms(&self) -> Array<Symbol> {
        // "MSSBCBS" is what real Azure SDKs (and the Functions Service Bus trigger) request
        // when authenticating with a SharedAccessKeyName/SharedAccessKey pair - they then do a
        // CBS put-token exchange over a `$cbs` link instead of relying on the SASL layer
        // itself. PLAIN/ANONYMOUS are kept for other AMQP clients/tools that don't use CBS.
        Array::from(vec![
            Symbol::from("MSSBCBS"),
            Symbol::from("PLAIN"),
            Symbol::from("ANONYMOUS"),
        ])
    }

    fn on_init(&mut self, _init: SaslInit) -> SaslServerFrame {
        SaslServerFrame::Outcome(SaslOutcome {
            code: SaslCode::Ok,
            additional_data: None,
        })
    }

    fn on_response(&mut self, _response: SaslResponse) -> SaslServerFrame {
        SaslServerFrame::Outcome(SaslOutcome {
            code: SaslCode::Ok,
            additional_data: None,
        })
    }
}

/// Runs the plain (non-TLS) AMQP 1.0 listener until the process is killed or the socket errors
/// out. This is what a connection string with `UseDevelopmentEmulator=true` talks to - the
/// Azure SDKs skip TLS entirely for that auth style.
pub async fn run_amqp_server(broker: Broker, addr: SocketAddr) -> anyhow::Result<()> {
    let tcp_listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "AMQP 1.0 listener started");

    let connection_acceptor = Arc::new(
        ConnectionAcceptor::builder()
            .container_id("sbemu")
            .sasl_acceptor(AcceptAnyCredentials)
            .build(),
    );

    loop {
        let (stream, peer) = tcp_listener.accept().await?;
        let broker = broker.clone();
        let connection_acceptor = connection_acceptor.clone();
        tokio::spawn(async move {
            match connection_acceptor.accept(stream).await {
                Ok(connection) => handle_connection(broker, connection).await,
                Err(err) => tracing::warn!(%peer, ?err, "amqp connection handshake failed"),
            }
        });
    }
}

/// Runs the AMQPS (AMQP over TLS) listener until the process is killed or the socket errors
/// out. Azure SDK clients constructed from a `TokenCredential` (e.g. to locally replicate how
/// a deployed Function authenticates via Managed Identity) always require TLS - there's no
/// `UseDevelopmentEmulator`-style bypass for that construction path - so this listener exists
/// purely to give that auth style somewhere to connect. Once the TLS handshake completes,
/// SASL/CBS negotiation is identical to the plain listener: every credential is accepted
/// unconditionally, since this is a local, no-real-security emulator.
///
/// Real Azure Service Bus (and every other AMQPS implementation in practice) uses *implicit*
/// TLS on this dedicated port: the client starts the TLS handshake as the very first bytes on
/// the wire, with no AMQP-level protocol-header pre-negotiation first (that negotiated-TLS
/// dance is only needed when a single port must serve both plain and TLS connections). So,
/// unlike `run_amqp_server`, we terminate TLS ourselves before ever handing the stream to
/// fe2o3-amqp - using its own built-in `.tls_acceptor(...)` builder option instead would make
/// it wait for that protocol-header handshake first, which real clients never send, and every
/// connection would fail with a "Invalid protocol header" error (the raw TLS ClientHello bytes
/// misread as an AMQP header).
pub async fn run_amqps_server(
    broker: Broker,
    addr: SocketAddr,
    tls_acceptor: tokio_rustls::TlsAcceptor,
) -> anyhow::Result<()> {
    let tcp_listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "AMQPS (AMQP over TLS) listener started");

    let connection_acceptor = Arc::new(
        ConnectionAcceptor::builder()
            .container_id("sbemu")
            .sasl_acceptor(AcceptAnyCredentials)
            .build(),
    );

    loop {
        let (stream, peer) = tcp_listener.accept().await?;
        let broker = broker.clone();
        let connection_acceptor = connection_acceptor.clone();
        let tls_acceptor = tls_acceptor.clone();
        tokio::spawn(async move {
            let tls_stream = match tls_acceptor.accept(stream).await {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(%peer, ?err, "amqps TLS handshake failed");
                    return;
                }
            };
            match connection_acceptor.accept(tls_stream).await {
                Ok(connection) => handle_connection(broker, connection).await,
                Err(err) => tracing::warn!(%peer, ?err, "amqps connection handshake failed"),
            }
        });
    }
}

/// Shared per-connection handling: CBS put-token, session acceptance, and link (sender/
/// receiver) routing. Used by both the plain AMQP and AMQPS listeners - identical once the
/// transport-level handshake (TLS or not) has completed.
async fn handle_connection(broker: Broker, mut connection: fe2o3_amqp::acceptor::ListenerConnectionHandle) {
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


/// Whether a link's address refers to the AMQP CBS (claims-based-security) management node,
/// used by real SDKs to "put-token" (send a SAS token) after the SASL layer completes.
fn is_cbs_address(address: &str) -> bool {
    address.trim().trim_start_matches('$').eq_ignore_ascii_case("cbs")
}

/// Handles the CBS put-token request link (accepted as a [`Receiver`] since the client is the
/// sender). Since this is a permissive dev emulator with no real security to check, every
/// request is unconditionally accepted and acknowledged with a 202 status on the paired
/// response link stored in `reply_sender`.
async fn handle_cbs_requests(mut receiver: Receiver, reply_sender: Arc<Mutex<Option<Sender>>>) {
    loop {
        let delivery = match receiver.recv::<Body<Value>>().await {
            Ok(d) => d,
            Err(err) => {
                tracing::debug!(?err, "cbs request link closed");
                break;
            }
        };

        let request_message_id = delivery
            .message()
            .properties
            .as_ref()
            .and_then(|p| p.message_id.clone());
        let _ = receiver.accept(&delivery).await;

        let mut app_props = ApplicationProperties::default();
        app_props.0.insert("status-code".to_string(), SimpleValue::Int(202));
        app_props
            .0
            .insert("status-description".to_string(), SimpleValue::String("Accepted".to_string()));

        let response = Message {
            header: None,
            delivery_annotations: None,
            message_annotations: None,
            properties: Some(Properties {
                message_id: Some(format!("cbs-response-{}", uuid::Uuid::new_v4()).into()),
                correlation_id: request_message_id,
                ..Default::default()
            }),
            application_properties: Some(app_props),
            body: Body::<Value>::Empty,
            footer: None,
        };

        // The paired response link may not have attached yet - give it a brief window to do so.
        let mut sent = false;
        for _ in 0..40 {
            if reply_sender.lock().await.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        {
            let mut guard = reply_sender.lock().await;
            if let Some(sender) = guard.as_mut() {
                match sender.send(response).await {
                    Ok(_) => sent = true,
                    Err(err) => tracing::debug!(?err, "failed to send cbs put-token response"),
                }
            }
        }
        if !sent {
            tracing::warn!("cbs response link never attached; put-token request not acknowledged");
        }
    }
}

/// Resolves an AMQP node address (as set on the link's source/target) to a broker entity.
/// Supports plain queue names ("orders") and Service-Bus-style subscription addresses
/// ("events/Subscriptions/audit-log").
fn resolve_entity(broker: &Broker, address: &str) -> Option<EntityHandle> {
    let address = address.trim_matches('/');
    if let Some((topic, rest)) = address.split_once('/') {
        let rest = rest.trim_start_matches("Subscriptions/").trim_start_matches("subscriptions/");
        return broker.get_subscription(topic, rest);
    }
    broker.get_queue(address)
}

/// The AMQP filter descriptor Service Bus session receivers attach with. Its value is either
/// a specific session id (client wants that exact session) or null (client wants "whichever
/// session is next available").
const SESSION_FILTER_NAME: &str = "com.microsoft:session-filter";

/// Returns `Some(requested)` if `source` carries a session filter (`requested` being `None`
/// for "next available session" or `Some(id)` for a specific one), or `None` if `source`
/// doesn't request session semantics at all (a plain, non-session receiver).
fn requested_session(source: &fe2o3_amqp::types::messaging::Source) -> Option<Option<String>> {
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
enum SessionOutcome {
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

/// Reads the next incoming Attach performative directly (bypassing `LinkAcceptor::accept`'s
/// combined helper) so that, for a Service Bus session receiver, we can lock a session and
/// rewrite the Attach's `Source` filter with the concrete session id *before* the Attach
/// response is sent - `fe2o3-amqp` otherwise always echoes a non-dynamic Source back verbatim,
/// which would never tell the client which session (if any) it was granted.
///
/// How long a receiver link asking for a message session (a specific id, or "next available")
/// keeps polling before giving up and detaching. This runs in its own spawned task (see the call
/// site in `handle_connection`), so a slow poll only delays completion of THIS ONE link's attach,
/// and no longer blocks any other concurrent link's attach on the same AMQP session. Kept
/// generous (10 minutes) because a short bound here (previously 5s, run inline and blocking)
/// made `ServiceBusSessionProcessor`'s idle concurrent "next available session" listener slots
/// detach/reattach in a tight loop, spamming `Failed to retreive session ID from broker
/// (GeneralError)` client-side every few seconds even though nothing was actually wrong; there
/// just wasn't a session to hand out yet.
const SESSION_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SESSION_POLL_MAX_ATTEMPTS: u32 = 2_400; // 2_400 * 250ms = 10 minutes

/// Pre-processes an incoming Attach *before* it is handed to the [`LinkAcceptor`]: fills in a
/// missing `initial-delivery-count` (fe2o3-amqp otherwise hard-rejects such attaches), and, for
/// Service Bus session-filter receivers, polls the broker for a session lock and rewrites the
/// `Source` filter with the concrete granted id. Deliberately takes no `session`/`link_acceptor`
/// (unlike the single-call helper this replaces) so it can run in its own spawned task without
/// holding the exclusive `ListenerSessionHandle` borrow the caller needs to keep discovering/
/// finalizing OTHER concurrent attaches on the same AMQP session while this one is still polling.
async fn resolve_attach_session(
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

/// A remote sender attached to us: they push messages TO an entity. We accepted this as a
/// [`Receiver`] link endpoint.
async fn handle_incoming_sender_link(broker: Broker, mut receiver: Receiver) {
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

fn amqp_body_to_new_message(message: &Message<Body<Value>>) -> NewMessage {
    let body = match message.body.clone() {
        Body::Data(data) => data.into_iter().next().map(|d| d.0.to_vec()).unwrap_or_default(),
        Body::Sequence(_) => Vec::new(),
        Body::Value(v) => format!("{:?}", v.0).into_bytes(),
        Body::Empty => Vec::new(),
    };
    let mut new_msg = NewMessage {
        body,
        ..Default::default()
    };

    if let Some(props) = &message.properties {
        new_msg.message_id = props.message_id.as_ref().map(message_id_to_string);
        new_msg.correlation_id = props.correlation_id.as_ref().map(message_id_to_string);
        new_msg.content_type = props.content_type.as_ref().map(|c| c.to_string());
        new_msg.session_id = props.group_id.clone();
    }

    if let Some(app_props) = &message.application_properties {
        for (k, v) in app_props.0.iter() {
            new_msg.properties.insert(k.clone(), format!("{v:?}"));
        }
    }

    new_msg
}

/// A remote receiver attached to us: they want to pull messages FROM an entity. We
/// accepted this as a [`Sender`] link endpoint.
async fn handle_incoming_receiver_link(broker: Broker, mut sender: Sender, session_outcome: Option<SessionOutcome>) {
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
    // Attach (see `accept_link_with_session_rewrite`), so we just act on that decision here
    // instead of trying to lock a session a second time.
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

fn new_message_to_amqp(msg: &emu_servicebus_core::BrokeredMessage) -> Message<Body<Value>> {
    let mut props = Properties {
        message_id: Some(msg.message_id.clone().into()),
        ..Default::default()
    };
    if let Some(cid) = &msg.correlation_id {
        props.correlation_id = Some(cid.clone().into());
    }
    if let Some(ct) = &msg.content_type {
        props.content_type = Some(ct.clone().into());
    }
    props.group_id = msg.session_id.clone();

    let mut app_props = ApplicationProperties::default();
    for (k, v) in &msg.properties {
        app_props.0.insert(k.clone(), SimpleValue::String(v.clone()));
    }

    // The Azure SDK's `ServiceBusReceivedMessage.DeliveryCount` getter unconditionally casts
    // the underlying AMQP header's delivery-count (`(int)AmqpMessage.Header.DeliveryCount`)
    // with no null-check, so every message needs a Header section carrying a real count -
    // without it the client throws `InvalidOperationException: Nullable object must have a
    // value` while building the trigger input, before user code ever runs.
    let header = Header {
        delivery_count: msg.delivery_count,
        ..Default::default()
    };

    Message {
        header: Some(header),
        delivery_annotations: None,
        message_annotations: None,
        properties: Some(props),
        application_properties: Some(app_props),
        body: Body::Data(vec![fe2o3_amqp::types::messaging::Data(msg.body.clone().into())].into()),
        footer: None,
    }
}
