//! Container/blob HTTP route handlers (mounted under `/:id/containers/...` by [`crate::router`]).

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use emu_storage_blob_core::{ContainerSummary, CoreError};

use crate::registry::{require_engine, StorageRegistry};

pub(crate) async fn list_containers(
    State(registry): State<StorageRegistry>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ContainerSummary>>, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    Ok(Json(store.list_containers()))
}

#[derive(Deserialize)]
pub(crate) struct CreateContainerBody {
    name: String,
}

pub(crate) async fn create_container(
    State(registry): State<StorageRegistry>,
    Path(id): Path<String>,
    Json(body): Json<CreateContainerBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.create_container(&body.name) {
        Ok(()) => Ok(StatusCode::CREATED),
        Err(CoreError::ContainerAlreadyExists(name)) => {
            Err((StatusCode::CONFLICT, format!("container '{name}' already exists")))
        }
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

pub(crate) async fn delete_container(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_container(&name) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(CoreError::ContainerNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("container '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

pub(crate) async fn list_blobs(
    State(registry): State<StorageRegistry>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.list_blobs(&name) {
        Ok(blobs) => Ok(Json(blobs).into_response()),
        Err(CoreError::ContainerNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("container '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

pub(crate) async fn download_blob(
    State(registry): State<StorageRegistry>,
    Path((id, container, blob)): Path<(String, String, String)>,
) -> Result<Response, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.get_blob(&container, &blob) {
        Ok(entry) => {
            let mut response = entry.data.into_response();
            if let Ok(value) = HeaderValue::from_str(&entry.content_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            Ok(response)
        }
        Err(CoreError::ContainerNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("container '{name}' not found"))),
        Err(CoreError::BlobNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("blob '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}

pub(crate) async fn upload_blob(
    State(registry): State<StorageRegistry>,
    Path((id, container, blob)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    // `put_blob` auto-creates the container if it doesn't exist yet, so no separate
    // existence check/creation is needed here.
    store.put_blob(&container, &blob, body, content_type, HashMap::new());
    Ok(StatusCode::CREATED)
}

pub(crate) async fn delete_blob(
    State(registry): State<StorageRegistry>,
    Path((id, container, blob)): Path<(String, String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let engine = require_engine(&registry, &id)?;
    let store = engine.store().await.ok_or((StatusCode::CONFLICT, "instance is not running".to_string()))?;
    match store.delete_blob(&container, &blob) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(CoreError::ContainerNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("container '{name}' not found"))),
        Err(CoreError::BlobNotFound(name)) => Err((StatusCode::NOT_FOUND, format!("blob '{name}' not found"))),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
    }
}
