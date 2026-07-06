//! Azure Table Storage REST API wire protocol (OData JSON), implemented over
//! `emu-storage-table-core`'s in-memory [`TableStore`]. This is what lets an unmodified
//! `Azure.Data.Tables` SDK client talk to this emulator by pointing a connection string's
//! `TableEndpoint` at `http://127.0.0.1:{port}/{account}`, the same path-style convention
//! Azurite uses.
//!
//! Scope (v1, matching the project's "80-90% of normal flows" philosophy): table create/
//! delete/list, entity insert/insert-or-replace/insert-or-merge/update/merge/delete/get,
//! and querying a table filtered by an exact `PartitionKey eq '...'` (the one query shape
//! Durable Functions/most simple apps actually need). Deliberately out of scope: arbitrary
//! `$filter` expressions, `$select`/`$top`/continuation-token paging, batch (`$batch`)
//! transactions, and SAS signature validation - auth is fully permissive, like every other
//! emulated resource here.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use chrono::SecondsFormat;
use serde_json::{json, Map, Value};

use emu_storage_table_core::{CoreError, EntityView, TableStore};

/// Builds the axum router implementing the Table REST wire protocol over `store`. Bind this
/// to its own dedicated port per Storage account instance (see the unified `StorageEngine`
/// in `emu-storage-engine`), separate from the dashboard's own HTTP server.
pub fn router(store: TableStore) -> Router {
    Router::new()
        .route("/:account/Tables", axum::routing::post(create_table).get(list_tables))
        .route(
            "/:account/:resource",
            axum::routing::get(resource_get)
                .post(resource_post)
                .put(resource_put)
                .patch(resource_merge)
                .delete(resource_delete),
        )
        .with_state(store)
}

fn etag_headers(etag: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(axum::http::header::ETAG, HeaderValue::from_str(etag).unwrap_or_else(|_| HeaderValue::from_static("")));
    headers
}

fn entity_to_json(view: &EntityView) -> Value {
    let mut obj = Map::new();
    obj.insert("odata.etag".to_string(), json!(view.etag));
    obj.insert("PartitionKey".to_string(), json!(view.partition_key));
    obj.insert("RowKey".to_string(), json!(view.row_key));
    obj.insert("Timestamp".to_string(), json!(view.timestamp.to_rfc3339_opts(SecondsFormat::Micros, true)));
    for (k, v) in &view.properties {
        obj.insert(k.clone(), v.clone());
    }
    Value::Object(obj)
}

/// Pulls `PartitionKey`/`RowKey` out of an entity request body, returning the remaining
/// custom properties separately (they're stored via `TableStore` without those two reserved
/// keys mixed in, since the store already tracks them as the entity's own key).
fn split_entity_body(mut body: Map<String, Value>) -> Option<(String, String, Map<String, Value>)> {
    let partition_key = body.remove("PartitionKey")?.as_str()?.to_string();
    let row_key = body.remove("RowKey")?.as_str()?.to_string();
    body.remove("Timestamp");
    body.remove("odata.etag");
    body.remove("odata.type");
    Some((partition_key, row_key, body))
}

/// `Tables('Name')` is the REST shape for `Delete Table` - the one place a `Tables(...)`
/// resource segment means something other than an entity key.
fn parse_delete_table_name(resource: &str) -> Option<String> {
    let inner = resource.strip_prefix("Tables(")?.strip_suffix(')')?;
    Some(unquote(inner))
}

fn unquote(s: &str) -> String {
    s.trim_matches('\'').replace("''", "'")
}

/// Splits an entity resource segment (`TableName` or `TableName(PartitionKey='p',RowKey='r')`)
/// into the table name and, if present, the entity's key.
fn parse_resource(resource: &str) -> (String, Option<(String, String)>) {
    let Some(paren_start) = resource.find('(') else {
        return (resource.to_string(), None);
    };
    let table = resource[..paren_start].to_string();
    let inner = resource[paren_start + 1..].trim_end_matches(')');
    let mut partition_key = None;
    let mut row_key = None;
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("PartitionKey=") {
            partition_key = Some(unquote(v));
        } else if let Some(v) = part.strip_prefix("RowKey=") {
            row_key = Some(unquote(v));
        }
    }
    match (partition_key, row_key) {
        (Some(pk), Some(rk)) => (table, Some((pk, rk))),
        _ => (table, None),
    }
}

/// Best-effort OData `$filter` parser: only recognizes a single `PartitionKey eq '...'`
/// clause (optionally combined with other terms via `and`, which are simply ignored) - see
/// the crate-level doc comment for why this is the one query shape supported.
fn parse_partition_key_filter(filter: &str) -> Option<String> {
    let needle = "PartitionKey eq '";
    let start = filter.find(needle)? + needle.len();
    let end = filter[start..].find('\'')? + start;
    Some(filter[start..end].to_string())
}

fn core_error_status(err: &CoreError) -> StatusCode {
    match err {
        CoreError::TableAlreadyExists(_) => StatusCode::CONFLICT,
        CoreError::TableNotFound(_) => StatusCode::NOT_FOUND,
        CoreError::EntityAlreadyExists => StatusCode::CONFLICT,
        CoreError::EntityNotFound => StatusCode::NOT_FOUND,
        CoreError::ETagMismatch => StatusCode::PRECONDITION_FAILED,
    }
}

// ----------------------------------------------------------------- tables

async fn create_table(State(store): State<TableStore>, Json(body): Json<Value>) -> Response {
    let Some(name) = body.get("TableName").and_then(|v| v.as_str()) else {
        return (StatusCode::BAD_REQUEST, "missing TableName").into_response();
    };
    match store.create_table(name) {
        Ok(()) => (StatusCode::CREATED, Json(json!({ "TableName": name }))).into_response(),
        Err(err) => (core_error_status(&err), err.to_string()).into_response(),
    }
}

async fn list_tables(State(store): State<TableStore>) -> Json<Value> {
    let tables: Vec<Value> = store.list_tables().into_iter().map(|t| json!({ "TableName": t.name })).collect();
    Json(json!({ "value": tables }))
}

// ---------------------------------------------------------------- entities

/// `GET /{account}/{resource}` - either "query entities in a table" (bare table name,
/// optionally with `$filter`) or "get one entity" (table name with a `(PK,RK)` key).
async fn resource_get(State(store): State<TableStore>, Path((_account, resource)): Path<(String, String)>, Query(query): Query<HashMap<String, String>>) -> Response {
    let (table, key) = parse_resource(&resource);
    if let Some((partition_key, row_key)) = key {
        return match store.get_entity(&table, &partition_key, &row_key) {
            Ok(entity) => (StatusCode::OK, etag_headers(&entity.etag), Json(entity_to_json(&entity))).into_response(),
            Err(err) => (core_error_status(&err), err.to_string()).into_response(),
        };
    }

    let partition_key_filter = query.get("$filter").and_then(|f| parse_partition_key_filter(f));
    match store.query_entities(&table, partition_key_filter.as_deref()) {
        Ok(entities) => {
            let values: Vec<Value> = entities.iter().map(entity_to_json).collect();
            Json(json!({ "value": values })).into_response()
        }
        Err(err) => (core_error_status(&err), err.to_string()).into_response(),
    }
}

/// `POST /{account}/{table}` - `Insert Entity`. Responds `204 No Content` when the request
/// has `Prefer: return-no-content` (the `Azure.Data.Tables` SDK's default for `AddEntity`),
/// otherwise `201 Created` with the full entity body.
async fn resource_post(State(store): State<TableStore>, Path((_account, table)): Path<(String, String)>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let Some(obj) = body.as_object().cloned() else {
        return (StatusCode::BAD_REQUEST, "expected a JSON object").into_response();
    };
    let Some((partition_key, row_key, properties)) = split_entity_body(obj) else {
        return (StatusCode::BAD_REQUEST, "missing PartitionKey/RowKey").into_response();
    };

    match store.insert_entity(&table, &partition_key, &row_key, properties) {
        Ok(entity) => {
            let mut resp_headers = etag_headers(&entity.etag);
            let wants_no_content = headers
                .get("prefer")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.contains("return-no-content"))
                .unwrap_or(false);
            if wants_no_content {
                (StatusCode::NO_CONTENT, resp_headers).into_response()
            } else {
                resp_headers.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                (StatusCode::CREATED, resp_headers, Json(entity_to_json(&entity))).into_response()
            }
        }
        Err(err) => (core_error_status(&err), err.to_string()).into_response(),
    }
}

/// `PUT /{account}/{table}(PartitionKey='p',RowKey='r')` - `Insert Or Replace Entity` when
/// no specific `If-Match` etag is given (the common upsert case), or strict `Update Entity`
/// (must already exist, etag must match) when one is.
async fn resource_put(State(store): State<TableStore>, Path((_account, resource)): Path<(String, String)>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    upsert_or_update(store, resource, headers, body, false).await
}

/// `PATCH /{account}/{table}(PartitionKey='p',RowKey='r')` (or legacy `MERGE`) -
/// `Insert Or Merge Entity`/`Merge Entity`, same If-Match distinction as [`resource_put`].
async fn resource_merge(State(store): State<TableStore>, Path((_account, resource)): Path<(String, String)>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    upsert_or_update(store, resource, headers, body, true).await
}

async fn upsert_or_update(store: TableStore, resource: String, headers: HeaderMap, body: Value, merge: bool) -> Response {
    let (table, key) = parse_resource(&resource);
    let Some((partition_key, row_key)) = key else {
        return (StatusCode::BAD_REQUEST, "missing entity key in URL").into_response();
    };
    let Some(properties) = body.as_object().cloned() else {
        return (StatusCode::BAD_REQUEST, "expected a JSON object").into_response();
    };
    // A specific (non-"*") If-Match means "this must already exist with this exact etag" -
    // real Azure Table Storage's strict Update/Merge Entity. Anything else (no header, or
    // "*") is treated as an upsert, matching `Azure.Data.Tables`' `UpsertEntity` behavior.
    let if_match = headers.get(axum::http::header::IF_MATCH).and_then(|v| v.to_str().ok());
    let result = match if_match {
        Some(etag) if etag != "*" => store.update_entity(&table, &partition_key, &row_key, properties, merge, Some(etag)),
        _ => store.upsert_entity(&table, &partition_key, &row_key, properties, merge),
    };
    match result {
        Ok(entity) => (StatusCode::NO_CONTENT, etag_headers(&entity.etag)).into_response(),
        Err(err) => (core_error_status(&err), err.to_string()).into_response(),
    }
}

/// `DELETE /{account}/{resource}` - either `Delete Table` (`Tables('Name')`) or
/// `Delete Entity` (`TableName(PartitionKey='p',RowKey='r')`).
async fn resource_delete(State(store): State<TableStore>, Path((_account, resource)): Path<(String, String)>, headers: HeaderMap) -> Response {
    if let Some(table_name) = parse_delete_table_name(&resource) {
        return match store.delete_table(&table_name) {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(err) => (core_error_status(&err), err.to_string()).into_response(),
        };
    }

    let (table, key) = parse_resource(&resource);
    let Some((partition_key, row_key)) = key else {
        return (StatusCode::BAD_REQUEST, "missing entity key in URL").into_response();
    };
    let if_match = headers.get(axum::http::header::IF_MATCH).and_then(|v| v.to_str().ok());
    match store.delete_entity(&table, &partition_key, &row_key, if_match) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => (core_error_status(&err), err.to_string()).into_response(),
    }
}

