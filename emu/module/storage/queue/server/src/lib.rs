//! Azure Queue Storage REST API wire protocol, implemented over
//! `emu-storage-queue-core`'s in-memory [`QueueStore`]. This is what lets an unmodified
//! `Azure.Storage.Queues` SDK client (or the Azure Functions host's queue-trigger polling)
//! talk to this emulator by pointing a connection string's `QueueEndpoint` at
//! `http://127.0.0.1:{port}/{account}`, the same path-style convention Azurite uses.
//!
//! Scope (v1, matching the project's "80-90% of normal flows" philosophy): queue create/
//! delete/list, put/get/peek/delete/update message, clear messages. Deliberately out of
//! scope: queue metadata (`x-ms-meta-*`), SAS signature validation, and shared access
//! policies - auth is fully permissive, like every other emulated resource here.

mod xml;

use std::collections::HashMap;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use chrono::{DateTime, Utc};

use emu_storage_queue_core::{CoreError, MessageView, QueueStore};

fn http_date(dt: DateTime<Utc>) -> String {
    dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn request_id_headers(headers: &mut HeaderMap) {
    headers.insert(
        HeaderName::from_static("x-ms-request-id"),
        HeaderValue::from_str(&uuid::Uuid::new_v4().to_string()).unwrap(),
    );
    headers.insert(HeaderName::from_static("x-ms-version"), HeaderValue::from_static("2021-08-06"));
}

fn query_i64(query: &HashMap<String, String>, key: &str) -> Option<i64> {
    query.get(key).and_then(|v| v.parse().ok())
}

fn query_usize(query: &HashMap<String, String>, key: &str, default: usize) -> usize {
    query.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Builds the axum router implementing the Queue REST wire protocol over `store`. Bind this
/// to its own dedicated port per Storage account instance (see the unified `StorageEngine`
/// in `emu-storage-blob-engine`), separate from the dashboard's own HTTP server.
pub fn router(store: QueueStore) -> Router {
    Router::new()
        .route("/:account", get(list_queues))
        .route("/:account/:queue", axum::routing::put(queue_put).delete(queue_delete).get(queue_get))
        .route(
            "/:account/:queue/messages",
            axum::routing::get(messages_get).post(messages_post).delete(messages_clear),
        )
        .route(
            "/:account/:queue/messages/:message_id",
            axum::routing::put(message_update).delete(message_delete),
        )
        .with_state(store)
}

// ---------------------------------------------------------------- queues

async fn list_queues(State(store): State<QueueStore>, Path(account): Path<String>) -> Response {
    let queues = store.list_queues();
    let mut body = String::new();
    body.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    body.push_str(&format!(
        "<EnumerationResults ServiceEndpoint=\"http://127.0.0.1/{}\">\n  <Queues>\n",
        xml::escape(&account)
    ));
    for q in &queues {
        body.push_str(&format!("    <Queue>\n      <Name>{}</Name>\n    </Queue>\n", xml::escape(&q.name)));
    }
    body.push_str("  </Queues>\n  <NextMarker/>\n</EnumerationResults>");

    let mut headers = HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/xml"));
    request_id_headers(&mut headers);
    (StatusCode::OK, headers, body).into_response()
}

async fn queue_put(State(store): State<QueueStore>, Path((_account, queue)): Path<(String, String)>) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    match store.create_queue(&queue) {
        Ok(()) => (StatusCode::CREATED, headers).into_response(),
        // A "create if not exists" caller (the common SDK pattern) treats 409 as success, so
        // this is a normal outcome, not an error - matches `emu-storage-blob-server`'s
        // container-create behavior.
        Err(CoreError::QueueAlreadyExists(_)) => (StatusCode::CONFLICT, headers).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, headers, err.to_string()).into_response(),
    }
}

async fn queue_delete(State(store): State<QueueStore>, Path((_account, queue)): Path<(String, String)>) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    match store.delete_queue(&queue) {
        Ok(()) => (StatusCode::NO_CONTENT, headers).into_response(),
        Err(CoreError::QueueNotFound(_)) => (StatusCode::NOT_FOUND, headers).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, headers, err.to_string()).into_response(),
    }
}

/// Handles bare `GET /{account}/{queue}` (`comp=metadata` - get queue properties/existence,
/// reporting `x-ms-approximate-messages-count`). There's only one `GET` shape for a queue
/// (unlike containers, which also support `comp=list`), so this doesn't need to branch on
/// `comp` the way `emu-storage-blob-server`'s container handler does.
async fn queue_get(State(store): State<QueueStore>, Path((_account, queue)): Path<(String, String)>) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    let queues = store.list_queues();
    match queues.iter().find(|q| q.name == queue) {
        Some(q) => {
            headers.insert(
                HeaderName::from_static("x-ms-approximate-messages-count"),
                HeaderValue::from_str(&q.approximate_message_count.to_string()).unwrap(),
            );
            (StatusCode::OK, headers).into_response()
        }
        None => (StatusCode::NOT_FOUND, headers).into_response(),
    }
}

// --------------------------------------------------------------- messages

fn message_xml(m: &MessageView, include_body: bool) -> String {
    let mut item = String::new();
    item.push_str("  <QueueMessage>\n");
    item.push_str(&format!("    <MessageId>{}</MessageId>\n", xml::escape(&m.message_id)));
    item.push_str(&format!("    <InsertionTime>{}</InsertionTime>\n", http_date(m.insertion_time)));
    item.push_str(&format!("    <ExpirationTime>{}</ExpirationTime>\n", http_date(m.expiration_time)));
    if let Some(pop_receipt) = &m.pop_receipt {
        item.push_str(&format!("    <PopReceipt>{}</PopReceipt>\n", xml::escape(pop_receipt)));
    }
    if let Some(next_visible) = m.time_next_visible {
        item.push_str(&format!("    <TimeNextVisible>{}</TimeNextVisible>\n", http_date(next_visible)));
    }
    item.push_str(&format!("    <DequeueCount>{}</DequeueCount>\n", m.dequeue_count));
    if include_body {
        item.push_str(&format!("    <MessageText>{}</MessageText>\n", xml::escape(&m.body)));
    }
    item.push_str("  </QueueMessage>\n");
    item
}

fn messages_response(messages: &[MessageView], include_body: bool) -> Response {
    let mut body = String::new();
    body.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<QueueMessagesList>\n");
    for m in messages {
        body.push_str(&message_xml(m, include_body));
    }
    body.push_str("</QueueMessagesList>");

    let mut headers = HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/xml"));
    request_id_headers(&mut headers);
    (StatusCode::OK, headers, body).into_response()
}

/// Handles `GET /{account}/{queue}/messages` - both `Get Messages` (dequeue) and, when
/// `peekonly=true` is set, `Peek Messages` (read without leasing), matching the real Azure
/// Queue REST API's single-endpoint-with-query-flag design.
async fn messages_get(
    State(store): State<QueueStore>,
    Path((_account, queue)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let count = query_usize(&query, "numofmessages", 1).clamp(1, 32);
    let peek_only = query.get("peekonly").map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false);

    let result = if peek_only {
        store.peek_messages(&queue, count)
    } else {
        let visibility_timeout = query_i64(&query, "visibilitytimeout").unwrap_or(30);
        store.get_messages(&queue, count, visibility_timeout)
    };

    match result {
        Ok(messages) => messages_response(&messages, true),
        Err(CoreError::QueueNotFound(_)) => {
            let mut headers = HeaderMap::new();
            request_id_headers(&mut headers);
            (StatusCode::NOT_FOUND, headers).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

/// Handles `POST /{account}/{queue}/messages` (`Put Message`). `visibilitytimeout` (default
/// 0 = immediately visible) and `messagettl` (default 7 days; `-1` = never expires) are
/// optional query parameters, matching the real API.
async fn messages_post(
    State(store): State<QueueStore>,
    Path((_account, queue)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let text = String::from_utf8_lossy(&body);
    let Some(message_text) = xml::extract_message_text(&text) else {
        return (StatusCode::BAD_REQUEST, "missing <MessageText> in request body").into_response();
    };
    let visibility_timeout = query_i64(&query, "visibilitytimeout").unwrap_or(0);
    let ttl = query_i64(&query, "messagettl");

    match store.put_message(&queue, message_text, visibility_timeout, ttl) {
        Ok(message) => messages_response(&[message], false),
        Err(CoreError::QueueNotFound(_)) => {
            let mut headers = HeaderMap::new();
            request_id_headers(&mut headers);
            (StatusCode::NOT_FOUND, headers).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn messages_clear(State(store): State<QueueStore>, Path((_account, queue)): Path<(String, String)>) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    match store.clear_messages(&queue) {
        Ok(()) => (StatusCode::NO_CONTENT, headers).into_response(),
        Err(CoreError::QueueNotFound(_)) => (StatusCode::NOT_FOUND, headers).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, headers, err.to_string()).into_response(),
    }
}

async fn message_delete(
    State(store): State<QueueStore>,
    Path((_account, queue, message_id)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    let Some(pop_receipt) = query.get("popreceipt") else {
        return (StatusCode::BAD_REQUEST, headers, "missing popreceipt query parameter").into_response();
    };
    match store.delete_message(&queue, &message_id, pop_receipt) {
        Ok(()) => (StatusCode::NO_CONTENT, headers).into_response(),
        Err(CoreError::QueueNotFound(_) | CoreError::MessageNotFound(_)) => (StatusCode::NOT_FOUND, headers).into_response(),
        Err(CoreError::PopReceiptMismatch(_)) => (StatusCode::CONFLICT, headers).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, headers, err.to_string()).into_response(),
    }
}

/// Handles `PUT /{account}/{queue}/messages/{message_id}` (`Update Message`): extends (or
/// shortens) a leased message's visibility timeout and optionally replaces its body,
/// returning a fresh `x-ms-popreceipt`/`x-ms-time-next-visible` pair via headers (no body).
async fn message_update(
    State(store): State<QueueStore>,
    Path((_account, queue, message_id)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    let Some(pop_receipt) = query.get("popreceipt") else {
        return (StatusCode::BAD_REQUEST, headers, "missing popreceipt query parameter").into_response();
    };
    let visibility_timeout = query_i64(&query, "visibilitytimeout").unwrap_or(0);
    let text = String::from_utf8_lossy(&body);
    let new_body = xml::extract_message_text(&text);

    match store.update_message(&queue, &message_id, pop_receipt, visibility_timeout, new_body) {
        Ok(message) => {
            headers.insert(
                HeaderName::from_static("x-ms-popreceipt"),
                HeaderValue::from_str(&message.pop_receipt.unwrap_or_default()).unwrap(),
            );
            if let Some(next_visible) = message.time_next_visible {
                headers.insert(
                    HeaderName::from_static("x-ms-time-next-visible"),
                    HeaderValue::from_str(&http_date(next_visible)).unwrap(),
                );
            }
            (StatusCode::NO_CONTENT, headers).into_response()
        }
        Err(CoreError::QueueNotFound(_) | CoreError::MessageNotFound(_)) => (StatusCode::NOT_FOUND, headers).into_response(),
        Err(CoreError::PopReceiptMismatch(_)) => (StatusCode::CONFLICT, headers).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, headers, err.to_string()).into_response(),
    }
}

