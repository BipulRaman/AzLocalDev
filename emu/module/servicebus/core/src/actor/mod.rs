//! Per-entity (queue/subscription) actor: a background tokio task holding all the mutable
//! state for one queue or subscription, driven purely by [`Command`] messages sent through
//! an [`EntityHandle`]. Split by concern (was a single 767-line file):
//! - [`command`]: the `Command` protocol between a handle and its actor task.
//! - [`handle`]: [`EntityHandle`], the cloneable client-facing API.
//! - [`state`]: [`state::EntityState`], the pure synchronous state machine (message buckets,
//!   peek-lock/session-lock tracking) - no async/tokio/`Command` awareness at all.
//! - [`task`]: [`spawn_entity`], the actual tokio task loop wiring `Command` dispatch to
//!   `EntityState` methods.

mod command;
mod handle;
mod state;
mod task;

pub use command::{Command, ReceivedMessage};
pub use handle::EntityHandle;
pub use task::spawn_entity;
