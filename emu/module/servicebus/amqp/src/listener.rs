//! The two public AMQP listener entry points: plain AMQP (for `UseDevelopmentEmulator=true`
//! connection strings) and AMQPS/TLS (for `TokenCredential`/Managed-Identity-style clients).

use std::sync::Arc;

use tokio::net::TcpListener;

use fe2o3_amqp::acceptor::ConnectionAcceptor;

use emu_servicebus_core::Broker;

use crate::connection::handle_connection;
use crate::sasl::AcceptAnyCredentials;

/// Runs the plain (non-TLS) AMQP 1.0 listener until the process is killed or the socket errors
/// out. This is what a connection string with `UseDevelopmentEmulator=true` talks to - the
/// Azure SDKs skip TLS entirely for that auth style. Takes an already-bound `tcp_listener`
/// (rather than binding one itself) so the caller can bind synchronously and propagate a bind
/// failure (e.g. the port already being in use) as a real `start()` error instead of it only
/// ever surfacing as a background-logged error from inside a spawned task.
pub async fn run_amqp_server(broker: Broker, tcp_listener: TcpListener) -> anyhow::Result<()> {
    let addr = tcp_listener.local_addr()?;
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
    tcp_listener: TcpListener,
    tls_acceptor: tokio_rustls::TlsAcceptor,
) -> anyhow::Result<()> {
    let addr = tcp_listener.local_addr()?;
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
