//! Core domain model for the Azure Table Storage emulator.
//!
//! This crate has no networking/protocol code in it at all - it is a plain thread-safe
//! in-memory store of tables and entities. The wire protocol (Azure Table REST API's OData
//! JSON, over HTTP) lives in `emu-storage-table-server`, driven purely by this store's
//! methods. Like `emu-storage-queue-core`, table/entity contents ARE persisted to disk
//! across restarts, alongside Blob data (see `emu-storage-blob-engine`'s unified
//! `StorageEngine::start`/`stop`/autosave, which calls this store's [`TableStore::dump`]/
//! [`TableStore::restore`]).
//!
//! Query support is intentionally minimal: this crate only understands "give me every
//! entity in a table" and "give me the one entity at this exact PartitionKey/RowKey" -
//! parsing OData `$filter` expressions (`emu-storage-table-server`'s job) only ever
//! produces one of those two shapes today. Arbitrary property filters aren't supported.

mod error;
mod model;

pub use error::{CoreError, CoreResult};
pub use model::{EntityDump, EntityView, TableDump, TableStoreDump, TableSummary};

use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;
use serde_json::Map;

struct Entity {
    properties: Map<String, serde_json::Value>,
    timestamp: chrono::DateTime<Utc>,
    etag: String,
}

impl Entity {
    fn to_view(&self, partition_key: &str, row_key: &str) -> EntityView {
        EntityView {
            partition_key: partition_key.to_string(),
            row_key: row_key.to_string(),
            timestamp: self.timestamp,
            etag: self.etag.clone(),
            properties: self.properties.clone(),
        }
    }
}

fn new_etag() -> String {
    format!("\"0x{}\"", uuid::Uuid::new_v4().simple())
}

struct TableState {
    /// Keyed by `(PartitionKey, RowKey)` - Azure Table Storage's actual primary key.
    entities: DashMap<(String, String), Entity>,
}

/// Thread-safe, cheaply-cloneable store of every table/entity for one Storage account
/// emulator instance. All clones share the same underlying state.
#[derive(Clone, Default)]
pub struct TableStore {
    tables: Arc<DashMap<String, TableState>>,
}

impl TableStore {
    pub fn new() -> Self {
        Self::default()
    }

    // ----------------------------------------------------------------- tables

    pub fn create_table(&self, name: &str) -> CoreResult<()> {
        if self.tables.contains_key(name) {
            return Err(CoreError::TableAlreadyExists(name.to_string()));
        }
        self.tables.insert(name.to_string(), TableState { entities: DashMap::new() });
        Ok(())
    }

    pub fn table_exists(&self, name: &str) -> bool {
        self.tables.contains_key(name)
    }

    pub fn delete_table(&self, name: &str) -> CoreResult<()> {
        self.tables.remove(name).map(|_| ()).ok_or_else(|| CoreError::TableNotFound(name.to_string()))
    }

    pub fn list_tables(&self) -> Vec<TableSummary> {
        self.tables
            .iter()
            .map(|entry| TableSummary {
                name: entry.key().clone(),
                entity_count: entry.value().entities.len(),
            })
            .collect()
    }

    // --------------------------------------------------------------- entities

    /// Inserts a brand new entity - fails with [`CoreError::EntityAlreadyExists`] if one
    /// already exists at this PartitionKey/RowKey (matches real Azure Table Storage's
    /// `Insert Entity`, as opposed to `insert_or_replace_entity`'s upsert semantics).
    pub fn insert_entity(&self, table: &str, partition_key: &str, row_key: &str, properties: Map<String, serde_json::Value>) -> CoreResult<EntityView> {
        let state = self.tables.get(table).ok_or_else(|| CoreError::TableNotFound(table.to_string()))?;
        let key = (partition_key.to_string(), row_key.to_string());
        if state.entities.contains_key(&key) {
            return Err(CoreError::EntityAlreadyExists);
        }
        let entity = Entity {
            properties,
            timestamp: Utc::now(),
            etag: new_etag(),
        };
        let view = entity.to_view(partition_key, row_key);
        state.entities.insert(key, entity);
        Ok(view)
    }

    /// Inserts a new entity, or replaces/merges an existing one - matches real Azure Table
    /// Storage's `Insert Or Replace Entity`/`Insert Or Merge Entity` (never fails just
    /// because the entity already exists, unlike [`Self::insert_entity`]/
    /// [`Self::update_entity`]).
    pub fn upsert_entity(&self, table: &str, partition_key: &str, row_key: &str, properties: Map<String, serde_json::Value>, merge: bool) -> CoreResult<EntityView> {
        let state = self.tables.get(table).ok_or_else(|| CoreError::TableNotFound(table.to_string()))?;
        let key = (partition_key.to_string(), row_key.to_string());
        let mut entry = state.entities.entry(key).or_insert_with(|| Entity {
            properties: Map::new(),
            timestamp: Utc::now(),
            etag: new_etag(),
        });
        if merge {
            for (k, v) in properties {
                entry.properties.insert(k, v);
            }
        } else {
            entry.properties = properties;
        }
        entry.timestamp = Utc::now();
        entry.etag = new_etag();
        Ok(entry.to_view(partition_key, row_key))
    }

    /// Replaces/merges an *existing* entity - fails with [`CoreError::EntityNotFound`] if it
    /// doesn't exist yet (matches real Azure Table Storage's `Update Entity`/`Merge Entity`).
    /// `if_match` is an optional `If-Match` precondition (an ETag, or `"*"` for
    /// unconditional) - a mismatch fails with [`CoreError::ETagMismatch`], matching the real
    /// API's optimistic-concurrency semantics.
    pub fn update_entity(
        &self,
        table: &str,
        partition_key: &str,
        row_key: &str,
        properties: Map<String, serde_json::Value>,
        merge: bool,
        if_match: Option<&str>,
    ) -> CoreResult<EntityView> {
        let state = self.tables.get(table).ok_or_else(|| CoreError::TableNotFound(table.to_string()))?;
        let key = (partition_key.to_string(), row_key.to_string());
        let mut entity = state.entities.get_mut(&key).ok_or(CoreError::EntityNotFound)?;
        if let Some(if_match) = if_match {
            if if_match != "*" && if_match != entity.etag {
                return Err(CoreError::ETagMismatch);
            }
        }
        if merge {
            for (k, v) in properties {
                entity.properties.insert(k, v);
            }
        } else {
            entity.properties = properties;
        }
        entity.timestamp = Utc::now();
        entity.etag = new_etag();
        Ok(entity.to_view(partition_key, row_key))
    }

    pub fn delete_entity(&self, table: &str, partition_key: &str, row_key: &str, if_match: Option<&str>) -> CoreResult<()> {
        let state = self.tables.get(table).ok_or_else(|| CoreError::TableNotFound(table.to_string()))?;
        let key = (partition_key.to_string(), row_key.to_string());
        if let Some(if_match) = if_match {
            if if_match != "*" {
                let entity = state.entities.get(&key).ok_or(CoreError::EntityNotFound)?;
                if if_match != entity.etag {
                    return Err(CoreError::ETagMismatch);
                }
            }
        }
        state.entities.remove(&key).map(|_| ()).ok_or(CoreError::EntityNotFound)
    }

    pub fn get_entity(&self, table: &str, partition_key: &str, row_key: &str) -> CoreResult<EntityView> {
        let state = self.tables.get(table).ok_or_else(|| CoreError::TableNotFound(table.to_string()))?;
        let key = (partition_key.to_string(), row_key.to_string());
        state
            .entities
            .get(&key)
            .map(|entity| entity.to_view(partition_key, row_key))
            .ok_or(CoreError::EntityNotFound)
    }

    /// Lists every entity in a table, optionally narrowed to one partition - the only two
    /// query shapes this emulator supports (see the crate-level doc comment).
    pub fn query_entities(&self, table: &str, partition_key: Option<&str>) -> CoreResult<Vec<EntityView>> {
        let state = self.tables.get(table).ok_or_else(|| CoreError::TableNotFound(table.to_string()))?;
        Ok(state
            .entities
            .iter()
            .filter(|entry| partition_key.is_none_or(|pk| entry.key().0 == pk))
            .map(|entry| entry.value().to_view(&entry.key().0, &entry.key().1))
            .collect())
    }

    // ------------------------------------------------------------ persistence

    /// Captures the whole store as a serializable [`TableStoreDump`].
    pub fn dump(&self) -> TableStoreDump {
        let mut tables: Vec<TableDump> = self
            .tables
            .iter()
            .map(|entry| {
                let name = entry.key().clone();
                let entities = entry
                    .value()
                    .entities
                    .iter()
                    .map(|e| {
                        let (partition_key, row_key) = e.key().clone();
                        let entity = e.value();
                        EntityDump {
                            partition_key,
                            row_key,
                            timestamp: entity.timestamp,
                            etag: entity.etag.clone(),
                            properties: entity.properties.clone(),
                        }
                    })
                    .collect();
                TableDump { name, entities }
            })
            .collect();
        tables.sort_by(|a, b| a.name.cmp(&b.name));
        TableStoreDump { tables }
    }

    /// Rebuilds a store from a previously-captured [`TableStoreDump`] (loaded from disk).
    pub fn restore(dump: TableStoreDump) -> Self {
        let store = Self::new();
        for table in dump.tables {
            let entities = DashMap::new();
            for e in table.entities {
                entities.insert(
                    (e.partition_key, e.row_key),
                    Entity {
                        properties: e.properties,
                        timestamp: e.timestamp,
                        etag: e.etag,
                    },
                );
            }
            store.tables.insert(table.name, TableState { entities });
        }
        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn insert_get_delete_roundtrip() {
        let store = TableStore::new();
        store.create_table("Orders").unwrap();
        let mut props = Map::new();
        props.insert("Status".to_string(), json!("Pending"));
        store.insert_entity("Orders", "customer-1", "order-1", props).unwrap();

        let got = store.get_entity("Orders", "customer-1", "order-1").unwrap();
        assert_eq!(got.properties.get("Status").unwrap(), "Pending");

        store.delete_entity("Orders", "customer-1", "order-1", None).unwrap();
        assert!(matches!(store.get_entity("Orders", "customer-1", "order-1"), Err(CoreError::EntityNotFound)));
    }

    #[test]
    fn insert_twice_fails_but_upsert_succeeds() {
        let store = TableStore::new();
        store.create_table("Orders").unwrap();
        store.insert_entity("Orders", "p", "r", Map::new()).unwrap();
        assert!(matches!(store.insert_entity("Orders", "p", "r", Map::new()), Err(CoreError::EntityAlreadyExists)));
        store.upsert_entity("Orders", "p", "r", Map::new(), false).unwrap();
    }

    #[test]
    fn merge_preserves_other_properties() {
        let store = TableStore::new();
        store.create_table("Orders").unwrap();
        let mut props = Map::new();
        props.insert("A".to_string(), json!(1));
        props.insert("B".to_string(), json!(2));
        store.insert_entity("Orders", "p", "r", props).unwrap();

        let mut update = Map::new();
        update.insert("B".to_string(), json!(3));
        store.update_entity("Orders", "p", "r", update, true, None).unwrap();

        let got = store.get_entity("Orders", "p", "r").unwrap();
        assert_eq!(got.properties.get("A").unwrap(), 1);
        assert_eq!(got.properties.get("B").unwrap(), 3);
    }

    #[test]
    fn query_by_partition_key() {
        let store = TableStore::new();
        store.create_table("Orders").unwrap();
        store.insert_entity("Orders", "p1", "r1", Map::new()).unwrap();
        store.insert_entity("Orders", "p2", "r1", Map::new()).unwrap();
        assert_eq!(store.query_entities("Orders", None).unwrap().len(), 2);
        assert_eq!(store.query_entities("Orders", Some("p1")).unwrap().len(), 1);
    }
}

