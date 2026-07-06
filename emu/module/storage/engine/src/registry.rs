//! Per-instance registry used by this module's own axum routes to resolve a request's
//! [`StorageEngine`] by id, without downcasting the generic `emu_registry::EmulatorEngine`
//! trait object.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use axum::http::StatusCode;
use emu_registry::EmulatorEngine;

use crate::engine::StorageEngine;

/// Thread-safe lookup table of `instance id -> StorageEngine`, used by this module's axum
/// routes to resolve which instance a request is for (see [`crate::router`]). Kept separate
/// from the generic [`emu_registry::EngineRegistry`] so route handlers can call
/// `StorageEngine`-specific methods directly without downcasting.
#[derive(Clone, Default)]
pub struct StorageRegistry {
    inner: Arc<StdMutex<HashMap<String, Arc<StorageEngine>>>>,
}

impl StorageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, engine: Arc<StorageEngine>) {
        self.inner.lock().unwrap().insert(engine.id().to_string(), engine);
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    pub fn get(&self, id: &str) -> Option<Arc<StorageEngine>> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    /// Every currently-registered Storage (Blob) instance - used at startup to bump the
    /// dashboard's port counter past whatever was just restored from a saved group, so
    /// newly-created instances afterward can't collide with restored ones.
    pub fn all(&self) -> Vec<Arc<StorageEngine>> {
        self.inner.lock().unwrap().values().cloned().collect()
    }
}

pub(crate) fn require_engine(registry: &StorageRegistry, id: &str) -> Result<Arc<StorageEngine>, (StatusCode, String)> {
    registry.get(id).ok_or((StatusCode::NOT_FOUND, format!("unknown Storage instance '{id}'")))
}
