//! Core domain model for the Storage (Blob) emulator.
//!
//! This crate has no networking/protocol code in it at all - it is a plain thread-safe
//! in-memory store of containers and blobs. The wire protocol (Azure Blob REST API over
//! HTTP) lives in `emu-storage-blob-server`, driven purely by this store's methods.

mod error;
mod model;

pub use error::{CoreError, CoreResult};
pub use model::{BlobDump, BlobEntry, BlobSummary, ContainerDump, ContainerSummary, StoreDump};

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use dashmap::DashMap;
use uuid::Uuid;

struct ContainerState {
    created_at: chrono::DateTime<chrono::Utc>,
    blobs: DashMap<String, BlobEntry>,
}

/// Thread-safe, cheaply-cloneable store of every container/blob for one Storage account
/// emulator instance. All clones share the same underlying state.
#[derive(Clone, Default)]
pub struct BlobStore {
    containers: Arc<DashMap<String, ContainerState>>,
}

fn new_etag() -> String {
    format!("\"0x{}\"", Uuid::new_v4().simple())
}

impl BlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------------------- containers

    pub fn create_container(&self, name: &str) -> CoreResult<()> {
        if self.containers.contains_key(name) {
            return Err(CoreError::ContainerAlreadyExists(name.to_string()));
        }
        self.containers.insert(
            name.to_string(),
            ContainerState {
                created_at: Utc::now(),
                blobs: DashMap::new(),
            },
        );
        Ok(())
    }

    pub fn container_exists(&self, name: &str) -> bool {
        self.containers.contains_key(name)
    }

    pub fn delete_container(&self, name: &str) -> CoreResult<()> {
        self.containers
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| CoreError::ContainerNotFound(name.to_string()))
    }

    pub fn list_containers(&self) -> Vec<ContainerSummary> {
        self.containers
            .iter()
            .map(|entry| ContainerSummary {
                name: entry.key().clone(),
                created_at: entry.value().created_at,
                blob_count: entry.value().blobs.len(),
            })
            .collect()
    }

    // ------------------------------------------------------------------ blobs

    /// Uploads (creates or overwrites) a blob. Auto-creates the container if it doesn't
    /// already exist, matching Azurite/real-Storage-adjacent developer convenience (real
    /// Azure requires the container to exist first, but emulator callers - especially the
    /// Functions host bootstrapping its own `azure-webjobs-*` containers - benefit from not
    /// having to special-case a first-run "create container" step).
    pub fn put_blob(
        &self,
        container: &str,
        blob: &str,
        data: Bytes,
        content_type: String,
        metadata: HashMap<String, String>,
    ) -> BlobSummary {
        let state = self.containers.entry(container.to_string()).or_insert_with(|| ContainerState {
            created_at: Utc::now(),
            blobs: DashMap::new(),
        });
        let entry = BlobEntry {
            name: blob.to_string(),
            content_type,
            etag: new_etag(),
            last_modified: Utc::now(),
            metadata,
            data,
        };
        let summary = BlobSummary {
            name: entry.name.clone(),
            content_type: entry.content_type.clone(),
            content_length: entry.data.len() as u64,
            etag: entry.etag.clone(),
            last_modified: entry.last_modified,
        };
        state.blobs.insert(blob.to_string(), entry);
        summary
    }

    pub fn get_blob(&self, container: &str, blob: &str) -> CoreResult<BlobEntry> {
        let state = self
            .containers
            .get(container)
            .ok_or_else(|| CoreError::ContainerNotFound(container.to_string()))?;
        state
            .blobs
            .get(blob)
            .map(|b| b.clone())
            .ok_or_else(|| CoreError::BlobNotFound(blob.to_string()))
    }

    pub fn delete_blob(&self, container: &str, blob: &str) -> CoreResult<()> {
        let state = self
            .containers
            .get(container)
            .ok_or_else(|| CoreError::ContainerNotFound(container.to_string()))?;
        state
            .blobs
            .remove(blob)
            .map(|_| ())
            .ok_or_else(|| CoreError::BlobNotFound(blob.to_string()))
    }

    pub fn list_blobs(&self, container: &str) -> CoreResult<Vec<BlobSummary>> {
        let state = self
            .containers
            .get(container)
            .ok_or_else(|| CoreError::ContainerNotFound(container.to_string()))?;
        let mut out: Vec<BlobSummary> = state
            .blobs
            .iter()
            .map(|entry| {
                let b = entry.value();
                BlobSummary {
                    name: b.name.clone(),
                    content_type: b.content_type.clone(),
                    content_length: b.data.len() as u64,
                    etag: b.etag.clone(),
                    last_modified: b.last_modified,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    // ------------------------------------------------------------ persistence

    /// Captures the whole store as a serializable [`StoreDump`].
    pub fn dump(&self) -> StoreDump {
        let mut containers: Vec<ContainerDump> = self
            .containers
            .iter()
            .map(|entry| {
                let name = entry.key().clone();
                let state = entry.value();
                let mut blobs: Vec<BlobDump> = state
                    .blobs
                    .iter()
                    .map(|b| {
                        let b = b.value();
                        BlobDump {
                            name: b.name.clone(),
                            content_type: b.content_type.clone(),
                            etag: b.etag.clone(),
                            last_modified: b.last_modified,
                            metadata: b.metadata.clone(),
                            data: b.data.clone(),
                        }
                    })
                    .collect();
                blobs.sort_by(|a, b| a.name.cmp(&b.name));
                ContainerDump {
                    name,
                    created_at: state.created_at,
                    blobs,
                }
            })
            .collect();
        containers.sort_by(|a, b| a.name.cmp(&b.name));
        StoreDump { containers }
    }

    /// Rebuilds a store from a previously-captured [`StoreDump`] (loaded from disk).
    pub fn restore(dump: StoreDump) -> Self {
        let store = Self::new();
        for container in dump.containers {
            let blobs = DashMap::new();
            for blob in container.blobs {
                blobs.insert(
                    blob.name.clone(),
                    BlobEntry {
                        name: blob.name,
                        content_type: blob.content_type,
                        etag: blob.etag,
                        last_modified: blob.last_modified,
                        metadata: blob.metadata,
                        data: blob.data,
                    },
                );
            }
            store.containers.insert(
                container.name,
                ContainerState {
                    created_at: container.created_at,
                    blobs,
                },
            );
        }
        store
    }
}
