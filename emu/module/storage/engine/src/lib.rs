//! The unified Azure Storage emulator module: owns a Blob store + Queue store + Table
//! store, each with its own HTTP listener speaking the real respective REST wire protocol
//! (the [`StorageEngine`], implementing the generic `EmulatorEngine` trait from
//! `emu-registry`), plus this module's own axum [`router`] exposing the container/blob,
//! queue/message, and table/entity data APIs that the dashboard UI nests under
//! `/api/storage`.
//!
//! One instance = one emulated Storage *account*, exactly like a real Azure Storage account
//! or an Azurite instance: three sequential ports starting at the instance's base port
//! (`base` = Blob, `base+1` = Queue, `base+2` = Table), matching Azurite's own
//! `10000`/`10001`/`10002` convention. Blob/Queue/Table contents are ALL persisted to disk
//! across restarts, in one combined JSON file per instance (see `engine::StorageDump`,
//! persisted via the shared `emu-persistence` crate) - the only thing that doesn't survive a
//! restart is an in-flight queue-message lease (`pop_receipt`), see
//! `emu-storage-queue-core::model::MessageDump`'s doc comment.
//!
//! Follows the same template as `emu-servicebus-engine`: a self-contained crate providing
//! an `EmulatorEngine` impl + its own API routes, with `emu/services/engine` and
//! `emu/ui/*` staying untouched. Internally split by concern (mirrors the crate-level
//! `core`/`server`/`engine` split every module uses, just as submodules instead of separate
//! crates since these are all small and unique to this one engine):
//! - `engine`: [`StorageEngine`] itself - lifecycle (start/stop), persistence, connection
//!   string/Managed-Identity config.
//! - `registry`: [`StorageRegistry`], the per-instance lookup table this module's routes use.
//! - `routes_blob`/`routes_queue`/`routes_table`: the axum handlers for each of the three
//!   REST surfaces, assembled together by [`router`] below.

mod engine;
mod registry;
mod routes_blob;
mod routes_queue;
mod routes_table;

pub use engine::StorageEngine;
pub use registry::StorageRegistry;

use axum::{routing::get, Router};

/// This module's own axum router (container/blob, queue/message, and table/entity data
/// APIs), keyed by instance id so the dashboard UI can address any number of Storage
/// instances the user has created. The dashboard UI mounts this under a path prefix (e.g.
/// `/api/storage-blob`) - route paths here are relative to that.
pub fn router(registry: StorageRegistry) -> Router {
    Router::new()
        .route("/:id/containers", get(routes_blob::list_containers).post(routes_blob::create_container))
        .route("/:id/containers/:name", axum::routing::delete(routes_blob::delete_container))
        .route("/:id/containers/:name/blobs", get(routes_blob::list_blobs))
        .route(
            "/:id/containers/:name/blobs/*blob",
            get(routes_blob::download_blob).put(routes_blob::upload_blob).delete(routes_blob::delete_blob),
        )
        .route("/:id/queues", get(routes_queue::list_queues).post(routes_queue::create_queue))
        .route("/:id/queues/:name", axum::routing::delete(routes_queue::delete_queue))
        .route(
            "/:id/queues/:name/messages",
            get(routes_queue::peek_queue_messages)
                .post(routes_queue::send_queue_message)
                .delete(routes_queue::clear_queue_messages),
        )
        .route("/:id/tables", get(routes_table::list_tables).post(routes_table::create_table))
        .route("/:id/tables/:name", axum::routing::delete(routes_table::delete_table))
        .route(
            "/:id/tables/:name/entities",
            get(routes_table::query_table_entities).post(routes_table::insert_table_entity),
        )
        .route(
            "/:id/tables/:name/entities/:partition_key/:row_key",
            axum::routing::delete(routes_table::delete_table_entity),
        )
        .with_state(registry)
}
