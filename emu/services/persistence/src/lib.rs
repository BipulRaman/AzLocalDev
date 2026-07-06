//! Shared per-instance JSON-file persistence, factored out of what used to be near-identical
//! copy-pasted boilerplate in `emu-servicebus-engine` and `emu-storage-engine` (each had
//! its own `data_dir()`/`sanitize_id()`/`load_dump()`/`save_*_state()`/autosave-ticker-loop).
//! Every future resource module (e.g. Cosmos, Event Hubs) should use this instead of
//! reimplementing the pattern again.
//!
//! Each engine module owns its own dump type (e.g. `BrokerDump`, or a combined
//! Blob+Queue+Table struct) and just calls [`load`]/[`save`]/[`spawn_autosave`] with it - this
//! crate has no knowledge of any concrete resource kind.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::task::JoinHandle;

/// Environment variable that, if set, overrides where [`data_dir`] resolves its base
/// directory from (instead of `dirs::config_dir()`) - used exclusively by integration tests
/// so they persist into an isolated temp directory instead of the real `%APPDATA%/AzLocalDev`
/// a developer might actually have data in. Never set by the shipped application itself.
pub const DATA_DIR_OVERRIDE_ENV: &str = "AZLOCALDEV_DATA_DIR_OVERRIDE";

/// Returns (creating on demand) the directory persisted data files for one resource module
/// live in: `%APPDATA%/AzLocalDev/data/{module_name}` (or the OS equivalent of
/// `dirs::config_dir()`), unless [`DATA_DIR_OVERRIDE_ENV`] is set. `module_name` should be a
/// short, stable, filesystem-safe slug like `"service-bus"` or `"storage"`.
pub fn data_dir(module_name: &str) -> PathBuf {
    let base = std::env::var_os(DATA_DIR_OVERRIDE_ENV)
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("AzLocalDev").join("data").join(module_name);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Makes an instance id safe to use as a filename: keeps alphanumerics, `-`, and `_`,
/// replaces everything else with `_`.
pub fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// The on-disk file one instance's data is persisted to, under `data_dir(module_name)`.
pub fn data_file(module_name: &str, id: &str) -> PathBuf {
    data_dir(module_name).join(format!("{}.json", sanitize_id(id)))
}

/// Generic on-disk envelope: stamps the owning instance's `id` alongside the actual dump
/// payload `T`, so persisted data is self-describing and can be verified against the
/// instance it's being loaded into instead of trusting the filename alone (e.g. in case a
/// file was ever copied/renamed by hand, or two groups' files got mixed up).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope<T> {
    id: String,
    #[serde(flatten)]
    dump: T,
}

/// Loads and verifies a previously-persisted dump of type `T` for `expected_id`, if present.
/// Returns `None` (logging a warning) if the file doesn't exist, fails to parse, or its
/// stamped id doesn't match `expected_id` - callers should fall back to a fresh/empty state
/// in every `None` case. `kind_label` is only used to make the log messages readable (e.g.
/// `"Service Bus"`, `"Storage"`).
pub fn load<T: DeserializeOwned>(path: &Path, expected_id: &str, kind_label: &str) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Envelope<T>>(&text) {
        Ok(data) => {
            if data.id != expected_id {
                tracing::warn!(
                    path = %path.display(),
                    stamped_id = %data.id,
                    %expected_id,
                    "persisted {kind_label} state's stamped id doesn't match this instance, refusing to load it"
                );
                return None;
            }
            Some(data.dump)
        }
        Err(err) => {
            tracing::warn!(?err, path = %path.display(), "failed to parse persisted {kind_label} state, starting empty");
            None
        }
    }
}

/// Serializes and writes `dump` (with `id` stamped alongside it - see [`load`]) to `path`.
/// Best-effort: logs a warning and otherwise does nothing on failure - persistence must
/// never take down the emulator.
pub fn save<T: Serialize>(path: &Path, id: &str, dump: T, kind_label: &str) {
    let data = Envelope { id: id.to_string(), dump };
    match serde_json::to_vec_pretty(&data) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(path, bytes) {
                tracing::warn!(?err, path = %path.display(), "failed to persist {kind_label} state");
            }
        }
        Err(err) => tracing::warn!(?err, "failed to serialize {kind_label} state"),
    }
}

/// Spawns the background task every engine uses as a crash-safety net: calls `save_fn` every
/// `interval`, skipping the very first tick (state is already fresh/empty or was just
/// restored right after `start()`, so there's nothing new to flush yet). `save_fn` is called
/// again from scratch each tick (rather than passed a single long-lived future), so it should
/// be a cheap closure that clones whatever `Arc`-backed handles it needs before producing its
/// future - exactly what every existing call site already did.
pub fn spawn_autosave<F, Fut>(interval: Duration, mut save_fn: F) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // first tick fires immediately; skip it
        loop {
            ticker.tick().await;
            save_fn().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct SampleDump {
        counter: u64,
        label: String,
    }

    /// Each test gets its own isolated temp directory (via [`DATA_DIR_OVERRIDE_ENV`]) so
    /// tests never touch a real `%APPDATA%/AzLocalDev` a developer might have data in.
    /// `cargo test` runs tests within a crate in parallel by default, and the override is a
    /// process-global env var, so a `Mutex` serializes every test that uses it - otherwise
    /// two tests running concurrently could stomp on each other's override.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TempDataDir {
        path: PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl TempDataDir {
        fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "azlocaldev-persistence-test-{}-{}",
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&path).unwrap();
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &path);
            Self { path, _guard: guard }
        }
    }

    impl Drop for TempDataDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let _dir = TempDataDir::new();
        let path = data_file("widgets", "widget-1");
        let dump = SampleDump { counter: 42, label: "hello".to_string() };

        save(&path, "widget-1", dump.clone(), "Widget");
        let loaded: Option<SampleDump> = load(&path, "widget-1", "Widget");

        assert_eq!(loaded, Some(dump));
    }

    #[test]
    fn load_rejects_mismatched_stamped_id() {
        let _dir = TempDataDir::new();
        let path = data_file("widgets", "widget-1");
        let dump = SampleDump { counter: 1, label: "x".to_string() };

        save(&path, "widget-1", dump, "Widget");
        // Loading under a DIFFERENT expected id must be rejected, even though the file exists.
        let loaded: Option<SampleDump> = load(&path, "widget-2", "Widget");

        assert_eq!(loaded, None);
    }

    #[test]
    fn load_returns_none_for_missing_file() {
        let _dir = TempDataDir::new();
        let path = data_file("widgets", "never-saved");
        let loaded: Option<SampleDump> = load(&path, "never-saved", "Widget");
        assert_eq!(loaded, None);
    }

    #[test]
    fn load_returns_none_for_malformed_json() {
        let _dir = TempDataDir::new();
        let path = data_file("widgets", "widget-1");
        std::fs::write(&path, "not valid json").unwrap();

        let loaded: Option<SampleDump> = load(&path, "widget-1", "Widget");
        assert_eq!(loaded, None);
    }

    #[test]
    fn sanitize_id_replaces_unsafe_characters() {
        assert_eq!(sanitize_id("service-bus_1"), "service-bus_1");
        assert_eq!(sanitize_id("weird/name:1"), "weird_name_1");
    }

    #[test]
    fn data_dir_is_isolated_by_module_name() {
        let _dir = TempDataDir::new();
        let a = data_dir("module-a");
        let b = data_dir("module-b");
        assert_ne!(a, b);
        assert!(a.ends_with("module-a"));
        assert!(b.ends_with("module-b"));
    }
}

