//! Table/entity HTTP route handlers (mounted under `/:id/tables/...` by [`crate::router`]).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use emu_storage_table_core::{CoreError as TableCoreError, EntityView, TableSummary};

use crate::registry::{require_engine, StorageRegistry};

pub(crate) async fn list_tables(State(registry): State<StorageRegistry>, Path(id): Path<String>) -> Result<Json<Vec<TableSummary>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.list_tables()))
}

#[derive(Deserialize)]
pub(crate) struct CreateTableBody {
    name: String,
}

pub(crate) async fn create_table(
    State(registry): State<StorageRegistry>,
    Path(id): Path<String>,
    Json(body): Json<CreateTableBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.create_table(&body.name) {
        Ok(()) => Ok(StatusCode::CREATED),
        Err(TableCoreError::TableAlreadyExists(name)) => Err((StatusCode::CONFLICT, format!("table '{name}' already exists"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

pub(crate) async fn delete_table(State(registry): State<StorageRegistry>, Path((id, name)): Path<(String, String)>) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_table(&name) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(TableCoreError::TableNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("table '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

#[derive(Deserialize)]
pub(crate) struct EntityQuery {
    partition_key: Option<String>,
}

pub(crate) async fn query_table_entities(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
    Query(query): Query<EntityQuery>,
) -> Result<Json<Vec<EntityView>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.query_entities(&name, query.partition_key.as_deref()) {
        Ok(entities) => Ok(Json(entities)),
        Err(TableCoreError::TableNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("table '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

#[derive(Deserialize)]
pub(crate) struct InsertEntityBody {
    partition_key: String,
    row_key: String,
    #[serde(default)]
    properties: serde_json::Map<String, serde_json::Value>,
}

pub(crate) async fn insert_table_entity(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<InsertEntityBody>,
) -> Result<Json<EntityView>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.insert_entity(&name, &body.partition_key, &body.row_key, body.properties) {
        Ok(entity) => Ok(Json(entity)),
        Err(TableCoreError::TableNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("table '{name}' not found"))),
        Err(TableCoreError::EntityAlreadyExists) => Err((StatusCode::CONFLICT, "entity already exists".to_string())),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

pub(crate) async fn delete_table_entity(
    State(registry): State<StorageRegistry>,
    Path((id, name, partition_key, row_key)): Path<(String, String, String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.table_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_entity(&name, &partition_key, &row_key, None) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(TableCoreError::TableNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("table '{name}' not found"))),
        Err(TableCoreError::EntityNotFound) => Err((StatusCode::NOT_FOUND, "entity not found".to_string())),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}
