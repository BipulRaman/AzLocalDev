//! Defines the `EmulatorEngine` trait that the dashboard uses to turn individual cloud
//! resource emulators on/off, plus the [`EngineRegistry`] that holds all of them and the
//! resource groups they're organized into.
//!
//! This crate is intentionally resource-agnostic: it knows nothing about Service Bus,
//! Storage Queues, or any other specific emulator. Each cloud resource emulator lives in
//! its own crate under `emu/module/<name>` and provides a concrete `EmulatorEngine` impl
//! (see `emu/module/servicebus/engine` for the first one). The dashboard (`emu/ui/*`) is
//! written only against this trait, so adding a new emulator never requires touching the
//! UI or control API code.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A single cloud resource emulator instance that can be turned on/off independently.
/// Users can create more than one instance of the same `kind` (e.g. two Service Bus
/// namespaces), each with its own `id`/`display_name` and its own underlying resources
/// (ports, broker, etc.) - it's up to each concrete impl to keep those from colliding.
#[async_trait]
pub trait EmulatorEngine: Send + Sync {
    /// Stable machine-readable identifier for this *instance*, e.g. "service-bus-1".
    /// Unique across the whole registry.
    fn id(&self) -> &str;
    /// Machine-readable identifier for the *type* of resource, e.g. "service-bus". Shared
    /// by every instance created from the same factory.
    fn kind(&self) -> &'static str;
    /// Human-readable name shown in the dashboard, e.g. "Orders Bus". Returns an owned
    /// `String` (rather than `&str`) since implementors store it behind interior mutability
    /// (e.g. a `Mutex<String>`) to support [`EmulatorEngine::rename`] - a lock guard can't be
    /// borrowed past the end of the call, so there's no `&str` to hand back.
    fn display_name(&self) -> String;
    /// Renames this instance in place. Implementors must accept this being called at any
    /// time, running or not.
    fn rename(&self, new_name: &str);
    /// Start the emulator. Must be idempotent (calling it while already running is a no-op).
    async fn start(&self) -> anyhow::Result<()>;
    /// Stop the emulator. Must be idempotent.
    async fn stop(&self) -> anyhow::Result<()>;
    /// Whether the emulator is currently running.
    async fn is_running(&self) -> bool;
    /// Short human-readable status line (e.g. connection string), shown in the dashboard
    /// when running.
    async fn detail(&self) -> Option<String>;
    /// Opaque, kind-specific config needed to recreate this instance later (e.g. the
    /// Service Bus engine returns its AMQP port). Used for session save/restore.
    fn config(&self) -> serde_json::Value;
}

/// A named container that resources are organized into, similar in spirit to a project
/// folder - purely organizational, has no behavior of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceGroup {
    pub id: String,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// JSON-serializable snapshot of one engine's state, for the dashboard's control API.
#[derive(Debug, Clone, Serialize)]
pub struct EngineSummary {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub group_id: String,
    pub running: bool,
    pub detail: Option<String>,
}

impl EngineSummary {
    pub async fn from_engine(engine: &dyn EmulatorEngine, group_id: String) -> Self {
        Self {
            id: engine.id().to_string(),
            kind: engine.kind().to_string(),
            display_name: engine.display_name(),
            group_id,
            running: engine.is_running().await,
            detail: engine.detail().await,
        }
    }
}

/// Describes one creatable resource *type* (e.g. "Service Bus"), for the dashboard's "New
/// resource" picker. The actual constructor lives in [`EngineRegistry`] as a factory
/// closure registered under the same `kind`, supplied by the composition root (the GUI),
/// which is the only place allowed to know about concrete per-module engine types.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceKind {
    pub kind: String,
    pub display_name: String,
}

type Factory =
    Arc<dyn Fn(String, String, Option<serde_json::Value>) -> Arc<dyn EmulatorEngine> + Send + Sync>;

struct RegistryState {
    engines: Vec<Arc<dyn EmulatorEngine>>,
    /// engine id -> resource group id
    engine_groups: HashMap<String, String>,
    groups: Vec<ResourceGroup>,
    kinds: Vec<ResourceKind>,
    factories: HashMap<String, Factory>,
    next_seq: u64,
    next_group_seq: u64,
}

/// Holds every registered engine *instance* and resource group so the dashboard can
/// list/start/stop/create them generically. Cheaply cloneable - all clones share the same
/// underlying state, so an instance created via one clone (e.g. inside an axum handler) is
/// visible to every other clone (e.g. the GUI's tray code) immediately.
#[derive(Clone)]
pub struct EngineRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl Default for EngineRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RegistryState {
                engines: Vec::new(),
                engine_groups: HashMap::new(),
                groups: Vec::new(),
                kinds: Vec::new(),
                factories: HashMap::new(),
                next_seq: 1,
                next_group_seq: 1,
            })),
        }
    }

    // ---------------------------------------------------------- resource groups

    /// Creates a new resource group and returns it. `id` lets callers pin a specific id
    /// (used when restoring a session); pass `None` to auto-generate one.
    pub fn create_group(&self, name: &str, id: Option<String>) -> ResourceGroup {
        let mut state = self.state.lock().unwrap();
        let id = id.unwrap_or_else(|| {
            let seq = state.next_group_seq;
            state.next_group_seq += 1;
            format!("group-{seq}")
        });
        if let Some(suffix) = id.strip_prefix("group-") {
            if let Ok(n) = suffix.parse::<u64>() {
                if n >= state.next_group_seq {
                    state.next_group_seq = n + 1;
                }
            }
        }
        let group = ResourceGroup {
            id,
            name: name.to_string(),
            created_at: chrono::Utc::now(),
        };
        state.groups.push(group.clone());
        group
    }

    pub fn list_groups(&self) -> Vec<ResourceGroup> {
        self.state.lock().unwrap().groups.clone()
    }

    pub fn group_of(&self, engine_id: &str) -> Option<String> {
        self.state.lock().unwrap().engine_groups.get(engine_id).cloned()
    }

    /// Renames a resource group in place.
    pub fn rename_group(&self, group_id: &str, new_name: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        let group = state
            .groups
            .iter_mut()
            .find(|g| g.id == group_id)
            .ok_or_else(|| anyhow::anyhow!("unknown resource group '{group_id}'"))?;
        group.name = new_name.to_string();
        Ok(())
    }

    /// Stops and removes every resource inside `group_id`, then removes the group itself.
    pub async fn delete_group(&self, group_id: &str) -> anyhow::Result<()> {
        let ids: Vec<String> = {
            let state = self.state.lock().unwrap();
            state
                .engine_groups
                .iter()
                .filter(|(_, g)| g.as_str() == group_id)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            self.remove(&id).await?;
        }
        self.state.lock().unwrap().groups.retain(|g| g.id != group_id);
        Ok(())
    }

    // ---------------------------------------------------------------- engines

    /// Registers an already-constructed engine instance (e.g. one created at startup) into
    /// `group_id`.
    pub fn register(&self, engine: Arc<dyn EmulatorEngine>, group_id: &str) {
        let mut state = self.state.lock().unwrap();
        state
            .engine_groups
            .insert(engine.id().to_string(), group_id.to_string());
        state.engines.push(engine);
    }

    /// Registers a factory for a creatable resource `kind`, so the dashboard's "New
    /// resource" flow can construct new instances of it by name alone. `display_name` is
    /// shown in the resource-type picker. The factory receives `Some(config)` when
    /// restoring a saved session, and `None` when creating a brand new instance (in which
    /// case the factory should pick its own defaults, e.g. auto-assign a port).
    pub fn register_kind(
        &self,
        kind: &str,
        display_name: &str,
        factory: impl Fn(String, String, Option<serde_json::Value>) -> Arc<dyn EmulatorEngine>
            + Send
            + Sync
            + 'static,
    ) {
        let mut state = self.state.lock().unwrap();
        state.kinds.push(ResourceKind {
            kind: kind.to_string(),
            display_name: display_name.to_string(),
        });
        state.factories.insert(kind.to_string(), Arc::new(factory));
    }

    pub fn kinds(&self) -> Vec<ResourceKind> {
        self.state.lock().unwrap().kinds.clone()
    }

    /// Creates and registers a new instance of `kind` named `name` inside `group_id`,
    /// auto-starts it, and returns it. The instance id is generated (`{kind}-{n}`).
    pub async fn create(
        &self,
        kind: &str,
        name: &str,
        group_id: &str,
    ) -> anyhow::Result<Arc<dyn EmulatorEngine>> {
        let (factory, id) = {
            let mut state = self.state.lock().unwrap();
            if !state.groups.iter().any(|g| g.id == group_id) {
                anyhow::bail!("unknown resource group '{group_id}'");
            }
            let factory = state
                .factories
                .get(kind)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown resource kind '{kind}'"))?;
            let seq = state.next_seq;
            state.next_seq += 1;
            (factory, format!("{kind}-{seq}"))
        };

        let engine = factory(id, name.to_string(), None);
        engine.start().await?;
        let mut state = self.state.lock().unwrap();
        state
            .engine_groups
            .insert(engine.id().to_string(), group_id.to_string());
        state.engines.push(engine.clone());
        Ok(engine)
    }

    /// Recreates a specific instance from a saved session (fixed id + config), auto-starts
    /// it, and registers it into `group_id`.
    pub async fn create_with_config(
        &self,
        kind: &str,
        id: String,
        name: &str,
        group_id: &str,
        config: serde_json::Value,
    ) -> anyhow::Result<Arc<dyn EmulatorEngine>> {
        let factory = {
            let mut state = self.state.lock().unwrap();
            bump_next_seq(&mut state.next_seq, kind, &id);
            state
                .factories
                .get(kind)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown resource kind '{kind}'"))?
        };

        let engine = factory(id, name.to_string(), Some(config));
        engine.start().await?;
        let mut state = self.state.lock().unwrap();
        state
            .engine_groups
            .insert(engine.id().to_string(), group_id.to_string());
        state.engines.push(engine.clone());
        Ok(engine)
    }

    /// Stops and removes every instance and every group. Used before restoring a saved
    /// session.
    pub async fn clear(&self) {
        for engine in self.all() {
            let _ = engine.stop().await;
        }
        let mut state = self.state.lock().unwrap();
        state.engines.clear();
        state.engine_groups.clear();
        state.groups.clear();
    }

    /// Stops and removes an instance from the registry.
    pub async fn remove(&self, id: &str) -> anyhow::Result<()> {
        let engine = {
            let mut state = self.state.lock().unwrap();
            let idx = state.engines.iter().position(|e| e.id() == id);
            let engine = idx.map(|i| state.engines.remove(i));
            state.engine_groups.remove(id);
            engine
        };
        if let Some(engine) = engine {
            engine.stop().await?;
        }
        Ok(())
    }

    pub fn all(&self) -> Vec<Arc<dyn EmulatorEngine>> {
        self.state.lock().unwrap().engines.clone()
    }

    pub fn in_group(&self, group_id: &str) -> Vec<Arc<dyn EmulatorEngine>> {
        let state = self.state.lock().unwrap();
        state
            .engines
            .iter()
            .filter(|e| state.engine_groups.get(e.id()).map(|g| g.as_str()) == Some(group_id))
            .cloned()
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn EmulatorEngine>> {
        self.state
            .lock()
            .unwrap()
            .engines
            .iter()
            .find(|e| e.id() == id)
            .cloned()
    }

    /// Renames an engine instance in place.
    pub fn rename(&self, id: &str, new_name: &str) -> anyhow::Result<()> {
        let engine = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown engine '{id}'"))?;
        engine.rename(new_name);
        Ok(())
    }

    pub async fn summaries(&self) -> Vec<EngineSummary> {
        let engines = self.all();
        let mut out = Vec::with_capacity(engines.len());
        for e in &engines {
            let group_id = self.group_of(e.id()).unwrap_or_default();
            out.push(EngineSummary::from_engine(e.as_ref(), group_id).await);
        }
        out
    }

    /// Stops every running engine. Used when the app is fully quitting (tray "Quit").
    pub async fn stop_all(&self) {
        for engine in self.all() {
            if let Err(err) = engine.stop().await {
                tracing::warn!(id = engine.id(), ?err, "failed to stop engine on shutdown");
            }
        }
    }

    // --------------------------------------------------------------- sessions

    /// Captures every current group and instance as a [`Session`] that can be serialized
    /// to disk and restored later with [`EngineRegistry::restore`].
    pub fn snapshot(&self, name: String) -> Session {
        let engines = self.all();
        let resources = engines
            .iter()
            .map(|e| SessionResource {
                id: e.id().to_string(),
                kind: e.kind().to_string(),
                name: e.display_name(),
                group_id: self.group_of(e.id()).unwrap_or_default(),
                config: e.config(),
            })
            .collect();
        Session {
            name,
            created_at: chrono::Utc::now(),
            groups: self.list_groups(),
            resources,
        }
    }

    /// Replaces every current group/instance with the ones described by `session`
    /// (stopping and discarding whatever is currently registered first).
    pub async fn restore(&self, session: &Session) -> anyhow::Result<()> {
        self.clear().await;
        for g in &session.groups {
            self.create_group(&g.name, Some(g.id.clone()));
        }
        for r in &session.resources {
            self.create_with_config(&r.kind, r.id.clone(), &r.name, &r.group_id, r.config.clone())
                .await?;
        }
        Ok(())
    }
}

fn bump_next_seq(next_seq: &mut u64, kind: &str, id: &str) {
    if let Some(suffix) = id.strip_prefix(kind).and_then(|s| s.strip_prefix('-')) {
        if let Ok(n) = suffix.parse::<u64>() {
            if n >= *next_seq {
                *next_seq = n + 1;
            }
        }
    }
}

/// One resource instance captured in a [`Session`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResource {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub group_id: String,
    pub config: serde_json::Value,
}

/// A named, point-in-time snapshot of every resource group and instance the user had set
/// up, so they can save it to disk (as JSON) and load it back later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub groups: Vec<ResourceGroup>,
    pub resources: Vec<SessionResource>,
}
