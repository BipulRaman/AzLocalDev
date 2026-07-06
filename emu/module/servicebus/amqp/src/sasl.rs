//! The SASL acceptor used by both AMQP listeners.

use fe2o3_amqp::acceptor::sasl_acceptor::{SaslAcceptor, SaslServerFrame};
use fe2o3_amqp::types::primitives::{Array, Symbol};
use fe2o3_amqp::types::sasl::{SaslCode, SaslInit, SaslOutcome, SaslResponse};

/// A dev-emulator SASL acceptor that authenticates every connection unconditionally.
/// Azure Service Bus connection strings always carry a `SharedAccessKeyName`/`SharedAccessKey`
/// pair, so real SDKs (and Azure Functions' Service Bus trigger) always negotiate SASL before
/// opening the AMQP connection. Since this is a local, no-real-security emulator, any
/// credentials are accepted - there is nothing to actually validate them against.
#[derive(Debug, Clone, Default)]
pub(crate) struct AcceptAnyCredentials;

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
