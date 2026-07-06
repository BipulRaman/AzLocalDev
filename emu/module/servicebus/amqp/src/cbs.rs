//! CBS (claims-based-security) put-token handling. Real Azure SDK clients (and the Functions
//! Service Bus trigger) authenticating with a SharedAccessKeyName/SharedAccessKey pair do a
//! CBS put-token exchange over a `$cbs` link instead of relying on the SASL layer alone.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use fe2o3_amqp::link::receiver::Receiver;
use fe2o3_amqp::link::sender::Sender;
use fe2o3_amqp::types::messaging::{ApplicationProperties, Body, Message, Properties};
use fe2o3_amqp::types::primitives::{SimpleValue, Value};

/// Whether a link's address refers to the AMQP CBS (claims-based-security) management node,
/// used by real SDKs to "put-token" (send a SAS token) after the SASL layer completes.
pub(crate) fn is_cbs_address(address: &str) -> bool {
    address.trim().trim_start_matches('$').eq_ignore_ascii_case("cbs")
}

/// Handles the CBS put-token request link (accepted as a [`Receiver`] since the client is the
/// sender). Since this is a permissive dev emulator with no real security to check, every
/// request is unconditionally accepted and acknowledged with a 202 status on the paired
/// response link stored in `reply_sender`.
pub(crate) async fn handle_cbs_requests(mut receiver: Receiver, reply_sender: Arc<Mutex<Option<Sender>>>) {
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
