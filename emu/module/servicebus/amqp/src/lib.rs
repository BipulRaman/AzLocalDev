//! AMQP 1.0 protocol adapter. Translates AMQP frames <-> [`emu_servicebus_core::Broker`]
//! commands. Contains no business logic beyond that translation. Split by concern (was a
//! single 651-line file):
//! - [`sasl`]: the permissive SASL acceptor every connection authenticates against.
//! - [`listener`]: [`run_amqp_server`]/[`run_amqps_server`], the two public entry points.
//! - [`connection`]: per-connection session/link accept loop.
//! - [`cbs`]: CBS (claims-based-security) put-token handling.
//! - [`session_filter`]: Service Bus message-session filter/lock resolution.
//! - [`links`]: per-link message pump (send/receive) once an attach is resolved.
//! - [`convert`]: AMQP wire type <-> domain type conversions.

mod cbs;
mod connection;
mod convert;
mod links;
mod listener;
mod sasl;
mod session_filter;

pub use listener::{run_amqp_server, run_amqps_server};

pub use emu_dev_cert::{load_or_generate as load_or_generate_dev_cert, DevCertificate};
