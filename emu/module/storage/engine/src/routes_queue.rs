//! Queue/message HTTP route handlers (mounted under `/:id/queues/...` by [`crate::router`]).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use emu_storage_queue_core::{CoreError as QueueCoreError, MessageView, QueueSummary};

use crate::registry::{require_engine, StorageRegistry};

pub(crate) async fn list_queues(State(registry): State<StorageRegistry>, Path(id): Path<String>) -> Result<Json<Vec<QueueSummary>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.list_queues()))
}

#[derive(Deserialize)]
pub(crate) struct CreateQueueBody {
    name: String,
}

pub(crate) async fn create_queue(
    State(registry): State<StorageRegistry>,
    Path(id): Path<String>,
    Json(body): Json<CreateQueueBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.create_queue(&body.name) {
        Ok(()) => Ok(StatusCode::CREATED),
        Err(QueueCoreError::QueueAlreadyExists(name)) => Err((StatusCode::CONFLICT, format!("queue '{name}' already exists"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

pub(crate) async fn delete_queue(State(registry): State<StorageRegistry>, Path((id, name)): Path<(String, String)>) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_queue(&name) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(QueueCoreError::QueueNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("queue '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

/// Dashboard "browse messages" - always a peek (never actually dequeues/leases anything),
/// same philosophy as the Azure Portal's own queue browser.
pub(crate) async fn peek_queue_messages(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<Vec<MessageView>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.peek_messages(&name, 32) {
        Ok(messages) => Ok(Json(messages)),
        Err(QueueCoreError::QueueNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("queue '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

#[derive(Deserialize)]
pub(crate) struct SendMessageBody {
    body: String,
}

pub(crate) async fn send_queue_message(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<SendMessageBody>,
) -> Result<Json<MessageView>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.put_message(&name, body.body, 0, None) {
        Ok(message) => Ok(Json(message)),
        Err(QueueCoreError::QueueNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("queue '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

pub(crate) async fn clear_queue_messages(State(registry): State<StorageRegistry>, Path((id, name)): Path<(String, String)>) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.queue_store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.clear_messages(&name) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(QueueCoreError::QueueNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("queue '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}
