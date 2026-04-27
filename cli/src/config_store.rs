//! Atomic edit + file-watch pipeline for `coulisse.yaml`.
//!
//! All config writes — admin UI saves, `PUT /admin/config`, hand-edits
//! in `$EDITOR` — go through this single point. The flow is the same
//! for every entry: read the file as a generic `serde_yaml::Value`,
//! splice in the change (for section writes), deserialize-and-validate
//! the result against [`Config`], then atomically replace the file
//! with a temp + rename. The on-disk file is the source of truth; the
//! file watcher reacts to every change (ours or external) and pushes
//! the new typed config through `on_reload` so feature crates' hot-
//! reloadable state (ArcSwap-wrapped agent lists, judge lists, etc.)
//! catches up immediately.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use coulisse_core::{ConfigPersistError, ConfigPersister};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::Mutex;

use crate::config::Config;

/// Callback invoked with a freshly validated [`Config`] every time the
/// file changes (admin save or external hand-edit). Async so feature
/// crates that need to read from the database (e.g. `agents` merging
/// YAML with `dynamic_agents` overrides) can do so before publishing
/// the new in-memory state.
pub type OnReload =
    Arc<dyn Fn(Config) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// Persists edits to the on-disk YAML and propagates reloads to
/// in-memory feature state. Held behind `Arc` so it can be passed to
/// every feature crate's admin router and to the cli's own
/// `PUT /admin/config` handler.
pub struct ConfigStore {
    /// Last validated `Config` snapshot. Updated on every successful
    /// admin write and every file-watcher reload. Admin handlers for
    /// sections that don't have their own `ArcSwap` (providers, mcp,
    /// memory, telemetry, auth) read straight off this — they're
    /// edited the same way as agents/judges/experiments but the
    /// runtime that consumes them only picks the change up on
    /// restart.
    snapshot: Arc<ArcSwap<Config>>,
    /// Serializes admin-side writes against each other so a section
    /// PUT and a whole-config PUT can't interleave between read and
    /// write. The file watcher does not contend with this lock — it
    /// only reads.
    write_lock: Mutex<()>,
    /// Path to `coulisse.yaml` (or whatever `COULISSE_CONFIG` was set
    /// to). Resolved to absolute at construction so relative-path
    /// surprises don't bite when the watcher fires from a different
    /// CWD context.
    path: PathBuf,
    on_reload: OnReload,
}

impl ConfigStore {
    pub fn new(path: PathBuf, initial: Config, on_reload: OnReload) -> Self {
        Self {
            on_reload,
            path,
            snapshot: Arc::new(ArcSwap::from_pointee(initial)),
            write_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Cheap O(1) clone of the last validated `Config`. Hot-edited
    /// sections see their changes here right after a successful save;
    /// sections that aren't hot-reloaded at runtime still get an
    /// up-to-date admin display.
    pub fn snapshot(&self) -> Arc<Config> {
        self.snapshot.load_full()
    }

    /// Spawn the filesystem watcher. Returns a guard that, when
    /// dropped, stops the watcher. Holding the guard for the lifetime
    /// of the process keeps reloads flowing.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub fn spawn_watcher(self: &Arc<Self>) -> Result<WatcherGuard, ConfigPersistError> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.send(event);
                }
            },
            notify::Config::default(),
        )
        .map_err(|err| ConfigPersistError::Io(err.to_string()))?;
        // Watch the parent directory rather than the file itself:
        // many editors save by writing a temp file + rename, which
        // breaks file-level inode watches on macOS/Linux. Filter
        // events down to our path on the receiving side.
        let watch_dir = self
            .path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        watcher
            .watch(&watch_dir, RecursiveMode::NonRecursive)
            .map_err(|err| ConfigPersistError::Io(err.to_string()))?;

        let store = Arc::clone(self);
        let target = self.path.clone();
        let handle = tokio::spawn(async move {
            // Coalesce bursts: editors typically emit several events
            // per save (truncate, write, rename, chmod). We only need
            // the trailing state, so wait a beat after the first event
            // and drain anything that piles up before reloading.
            const DEBOUNCE: Duration = Duration::from_millis(75);
            while let Some(event) = rx.recv().await {
                if !is_relevant(&event, &target) {
                    continue;
                }
                tokio::time::sleep(DEBOUNCE).await;
                while let Ok(extra) = rx.try_recv() {
                    let _ = extra;
                }
                if let Err(err) = store.reload_from_disk().await {
                    tracing::warn!(
                        error = %err,
                        path = %target.display(),
                        "config reload failed; keeping previous in-memory state"
                    );
                }
            }
        });

        Ok(WatcherGuard {
            _handle: handle,
            _watcher: watcher,
        })
    }

    /// Re-read the on-disk file, validate, and push the new config
    /// into feature crates via `on_reload`. Errors are reported but
    /// non-fatal — the previous in-memory state stays live so a broken
    /// hand-edit doesn't take chat down.
    async fn reload_from_disk(&self) -> Result<(), ConfigPersistError> {
        let path = self.path.clone();
        let config = tokio::task::spawn_blocking(move || Config::from_path(&path))
            .await
            .map_err(|err| ConfigPersistError::Io(err.to_string()))?
            .map_err(|err| ConfigPersistError::Invalid(err.to_string()))?;
        self.snapshot.store(Arc::new(config.clone()));
        (self.on_reload)(config).await;
        tracing::info!(path = %self.path.display(), "config reloaded");
        Ok(())
    }

    /// Read the file as a YAML mapping. Lets writers splice in a
    /// section without dropping unknown keys — future YAML fields a
    /// newer binary doesn't recognize round-trip cleanly.
    fn read_root(&self) -> Result<serde_yaml::Mapping, ConfigPersistError> {
        let bytes = fs::read(&self.path).map_err(|err| ConfigPersistError::Io(err.to_string()))?;
        let value: serde_yaml::Value = serde_yaml::from_slice(&bytes)
            .map_err(|err| ConfigPersistError::Parse(err.to_string()))?;
        match value {
            serde_yaml::Value::Mapping(m) => Ok(m),
            serde_yaml::Value::Null => Ok(serde_yaml::Mapping::new()),
            _ => Err(ConfigPersistError::Parse(
                "config root must be a YAML mapping".into(),
            )),
        }
    }

    /// Validate and atomically write a serialized YAML mapping to disk.
    /// Returns the parsed [`Config`] on success so callers can also
    /// trigger an immediate in-memory reload without waiting for the
    /// file watcher (small latency win on the admin response path).
    fn validate_and_write(&self, root: serde_yaml::Mapping) -> Result<Config, ConfigPersistError> {
        let value = serde_yaml::Value::Mapping(root);
        let config: Config = serde_yaml::from_value(value.clone())
            .map_err(|err| ConfigPersistError::Parse(err.to_string()))?;
        config
            .validate()
            .map_err(|err| ConfigPersistError::Invalid(err.to_string()))?;
        let serialized = serde_yaml::to_string(&value)
            .map_err(|err| ConfigPersistError::Parse(err.to_string()))?;
        write_atomically(&self.path, serialized.as_bytes())
            .map_err(|err| ConfigPersistError::Io(err.to_string()))?;
        self.snapshot.store(Arc::new(config.clone()));
        Ok(config)
    }
}

impl ConfigPersister for ConfigStore {
    fn write_section<'a>(
        &'a self,
        section: &'a str,
        value: serde_yaml::Value,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), ConfigPersistError>> + Send + 'a>>
    {
        Box::pin(async move {
            let _guard = self.write_lock.lock().await;
            let mut root = self.read_root()?;
            root.insert(serde_yaml::Value::String(section.to_string()), value);
            let config = self.validate_and_write(root)?;
            (self.on_reload)(config).await;
            Ok(())
        })
    }

    fn write_all<'a>(
        &'a self,
        value: serde_yaml::Value,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), ConfigPersistError>> + Send + 'a>>
    {
        Box::pin(async move {
            let _guard = self.write_lock.lock().await;
            let serde_yaml::Value::Mapping(root) = value else {
                return Err(ConfigPersistError::Parse(
                    "config root must be a YAML mapping".into(),
                ));
            };
            let config = self.validate_and_write(root)?;
            (self.on_reload)(config).await;
            Ok(())
        })
    }
}

/// Drop guard for the file watcher. Keeps the watcher and the watcher-
/// task handle alive while held; dropping aborts the task and the
/// watcher's OS handle.
pub struct WatcherGuard {
    _handle: tokio::task::JoinHandle<()>,
    _watcher: RecommendedWatcher,
}

fn is_relevant(event: &Event, target: &Path) -> bool {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return false;
    }
    event.paths.iter().any(|p| paths_equal(p, target))
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a)
        .ok()
        .zip(fs::canonicalize(b).ok())
        .map_or_else(|| a == b, |(ca, cb)| ca == cb)
}

/// Atomic write: temp file in the same directory + rename. Same-
/// directory rename is atomic on POSIX and on NTFS, so readers (the
/// file watcher, hand-running editors that re-open the file) never see
/// a half-written file.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .map_or_else(|| "config".into(), |s| s.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{stem}.coulisse-{}.tmp", std::process::id()));
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    let result = fs::rename(&tmp, path);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
