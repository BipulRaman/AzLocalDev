//! Azure Blob Storage REST API wire protocol, implemented over `emu-storage-blob-core`'s
//! in-memory [`BlobStore`]. This is what lets an unmodified `Azure.Storage.Blobs` SDK client
//! (or the Azure Functions host itself) talk to this emulator by pointing a connection
//! string's `BlobEndpoint` at `http://127.0.0.1:{port}/{account}`, the same path-style
//! convention Azurite uses.
//!
//! Scope (v1, matching the project's "80-90% of normal flows" philosophy): container
//! create/delete/list, single-shot block blob upload/download/delete/list, `x-ms-meta-*`
//! metadata. Deliberately out of scope: leases, snapshots/versioning, soft delete, SAS
//! signature validation, the block-list (`Put Block`/`Put Block List`) large-upload API,
//! and conditional (`If-Match`/`If-None-Match`) requests - auth is fully permissive, like the
//! Service Bus AMQP listener's SASL acceptor.

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

use emu_storage_blob_core::{BlobStore, CoreError};

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

fn metadata_from_headers(headers: &HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (name, value) in headers.iter() {
        if let Some(key) = name.as_str().strip_prefix("x-ms-meta-") {
            if let Ok(v) = value.to_str() {
                out.insert(key.to_string(), v.to_string());
            }
        }
    }
    out
}

/// Builds the axum router implementing the Blob REST wire protocol over `store`. Bind this
/// to its own dedicated port per Storage account instance (see `emu-storage-blob-engine`),
/// separate from the dashboard's own HTTP server.
pub fn router(store: BlobStore) -> Router {
    Router::new()
        .route("/:account", get(list_containers))
        .route(
            "/:account/:container",
            axum::routing::put(container_put)
                .delete(container_delete)
                .get(container_get)
                .head(container_head),
        )
        .route(
            "/:account/:container/*blob",
            axum::routing::put(blob_put)
                .get(blob_get)
                .head(blob_head)
                .delete(blob_delete),
        )
        .with_state(store)
}

// -------------------------------------------------------------- containers

async fn list_containers(
    State(store): State<BlobStore>,
    Path(account): Path<String>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    let containers = store.list_containers();
    let mut body = String::new();
    body.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    body.push_str(&format!(
        "<EnumerationResults ServiceEndpoint=\"http://127.0.0.1/{}\">\n  <Containers>\n",
        xml::escape(&account)
    ));
    for c in &containers {
        body.push_str(&format!(
            "    <Container>\n      <Name>{}</Name>\n      <Properties>\n        <Last-Modified>{}</Last-Modified>\n        <Etag>\"0x0\"</Etag>\n        <LeaseStatus>unlocked</LeaseStatus>\n        <LeaseState>available</LeaseState>\n      </Properties>\n    </Container>\n",
            xml::escape(&c.name),
            http_date(c.created_at)
        ));
    }
    body.push_str("  </Containers>\n  <NextMarker/>\n</EnumerationResults>");

    let mut headers = HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/xml"));
    request_id_headers(&mut headers);
    (StatusCode::OK, headers, body).into_response()
}

async fn container_put(State(store): State<BlobStore>, Path((_account, container)): Path<(String, String)>) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    match store.create_container(&container) {
        Ok(()) => (StatusCode::CREATED, headers).into_response(),
        // A "create if not exists" caller (the common SDK pattern) treats 409 as success, so
        // this is a normal outcome, not an error.
        Err(CoreError::ContainerAlreadyExists(_)) => (StatusCode::CONFLICT, headers).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, headers, err.to_string()).into_response(),
    }
}

async fn container_delete(State(store): State<BlobStore>, Path((_account, container)): Path<(String, String)>) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    match store.delete_container(&container) {
        Ok(()) => (StatusCode::ACCEPTED, headers).into_response(),
        Err(CoreError::ContainerNotFound(_)) => (StatusCode::NOT_FOUND, headers).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, headers, err.to_string()).into_response(),
    }
}

async fn container_get(
    State(store): State<BlobStore>,
    Path((account, container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);

    if query.get("comp").map(String::as_str) == Some("list") {
        let blobs = match store.list_blobs(&container) {
            Ok(b) => b,
            Err(CoreError::ContainerNotFound(_)) => return (StatusCode::NOT_FOUND, headers).into_response(),
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, headers, err.to_string()).into_response(),
        };
        let mut body = String::new();
        body.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        body.push_str(&format!(
            "<EnumerationResults ServiceEndpoint=\"http://127.0.0.1/{}\" ContainerName=\"{}\">\n  <Blobs>\n",
            xml::escape(&account),
            xml::escape(&container)
        ));
        for b in &blobs {
            body.push_str(&format!(
                "    <Blob>\n      <Name>{}</Name>\n      <Properties>\n        <Last-Modified>{}</Last-Modified>\n        <Etag>{}</Etag>\n        <Content-Length>{}</Content-Length>\n        <Content-Type>{}</Content-Type>\n        <BlobType>BlockBlob</BlobType>\n        <LeaseStatus>unlocked</LeaseStatus>\n        <LeaseState>available</LeaseState>\n      </Properties>\n    </Blob>\n",
                xml::escape(&b.name),
                http_date(b.last_modified),
                xml::escape(&b.etag),
                b.content_length,
                xml::escape(&b.content_type)
            ));
        }
        body.push_str("  </Blobs>\n  <NextMarker/>\n</EnumerationResults>");
        headers.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/xml"));
        return (StatusCode::OK, headers, body).into_response();
    }

    // Bare `restype=container` (get container properties / existence check).
    if store.container_exists(&container) {
        (StatusCode::OK, headers).into_response()
    } else {
        (StatusCode::NOT_FOUND, headers).into_response()
    }
}

async fn container_head(State(store): State<BlobStore>, Path((_account, container)): Path<(String, String)>) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    if store.container_exists(&container) {
        (StatusCode::OK, headers).into_response()
    } else {
        (StatusCode::NOT_FOUND, headers).into_response()
    }
}

// -------------------------------------------------------------------- blobs

async fn blob_put(
    State(store): State<BlobStore>,
    Path((_account, container, blob)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let metadata = metadata_from_headers(&headers);
    let summary = store.put_blob(&container, &blob, body, content_type, metadata);

    let mut out = HeaderMap::new();
    request_id_headers(&mut out);
    out.insert(
        axum::http::header::ETAG,
        HeaderValue::from_str(&summary.etag).unwrap_or_else(|_| HeaderValue::from_static("\"0x0\"")),
    );
    out.insert(
        HeaderName::from_static("last-modified"),
        HeaderValue::from_str(&http_date(summary.last_modified)).unwrap(),
    );
    (StatusCode::CREATED, out).into_response()
}

async fn blob_get(
    State(store): State<BlobStore>,
    Path((_account, container, blob)): Path<(String, String, String)>,
) -> Response {
    match store.get_blob(&container, &blob) {
        Ok(entry) => {
            let mut headers = HeaderMap::new();
            request_id_headers(&mut headers);
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_str(&entry.content_type).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );
            headers.insert(axum::http::header::CONTENT_LENGTH, HeaderValue::from(entry.data.len()));
            headers.insert(
                axum::http::header::ETAG,
                HeaderValue::from_str(&entry.etag).unwrap_or_else(|_| HeaderValue::from_static("\"0x0\"")),
            );
            headers.insert(
                HeaderName::from_static("last-modified"),
                HeaderValue::from_str(&http_date(entry.last_modified)).unwrap(),
            );
            headers.insert(HeaderName::from_static("x-ms-blob-type"), HeaderValue::from_static("BlockBlob"));
            for (k, v) in &entry.metadata {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::try_from(format!("x-ms-meta-{k}")),
                    HeaderValue::from_str(v),
                ) {
                    headers.insert(name, value);
                }
            }
            (StatusCode::OK, headers, entry.data).into_response()
        }
        Err(_not_found) => {
            let mut headers = HeaderMap::new();
            request_id_headers(&mut headers);
            (StatusCode::NOT_FOUND, headers).into_response()
        }
    }
}

async fn blob_head(
    State(store): State<BlobStore>,
    Path((_account, container, blob)): Path<(String, String, String)>,
) -> Response {
    match store.get_blob(&container, &blob) {
        Ok(entry) => {
            let mut headers = HeaderMap::new();
            request_id_headers(&mut headers);
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_str(&entry.content_type).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );
            headers.insert(axum::http::header::CONTENT_LENGTH, HeaderValue::from(entry.data.len()));
            headers.insert(
                axum::http::header::ETAG,
                HeaderValue::from_str(&entry.etag).unwrap_or_else(|_| HeaderValue::from_static("\"0x0\"")),
            );
            headers.insert(
                HeaderName::from_static("last-modified"),
                HeaderValue::from_str(&http_date(entry.last_modified)).unwrap(),
            );
            headers.insert(HeaderName::from_static("x-ms-blob-type"), HeaderValue::from_static("BlockBlob"));
            (StatusCode::OK, headers).into_response()
        }
        Err(_not_found) => {
            let mut headers = HeaderMap::new();
            request_id_headers(&mut headers);
            (StatusCode::NOT_FOUND, headers).into_response()
        }
    }
}

async fn blob_delete(
    State(store): State<BlobStore>,
    Path((_account, container, blob)): Path<(String, String, String)>,
) -> Response {
    let mut headers = HeaderMap::new();
    request_id_headers(&mut headers);
    match store.delete_blob(&container, &blob) {
        Ok(()) => (StatusCode::ACCEPTED, headers).into_response(),
        Err(_not_found) => (StatusCode::NOT_FOUND, headers).into_response(),
    }
}
