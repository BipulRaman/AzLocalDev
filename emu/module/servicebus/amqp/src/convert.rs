//! Conversions between AMQP wire types ([`fe2o3_amqp`]) and this crate's domain types
//! ([`emu_servicebus_core`]).

use emu_servicebus_core::{BrokeredMessage, NewMessage};
use fe2o3_amqp::types::messaging::{ApplicationProperties, Body, Header, Message, MessageId, Properties};
use fe2o3_amqp::types::primitives::{SimpleValue, Value};

pub(crate) fn message_id_to_string(id: &MessageId) -> String {
    format!("{id:?}")
}

pub(crate) fn amqp_body_to_new_message(message: &Message<Body<Value>>) -> NewMessage {
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

pub(crate) fn new_message_to_amqp(msg: &BrokeredMessage) -> Message<Body<Value>> {
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
