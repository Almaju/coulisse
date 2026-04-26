//! Trait for hot-editing the YAML config file.
//!
//! Feature crates need to write back to `coulisse.yaml` (admin UI form
//! save, API PUT, etc.) but don't own the file or the cross-feature
//! validation pipeline. This trait sits at that seam: the cli
//! implements it, every feature crate's admin router takes
//! `Arc<dyn ConfigPersister>` and calls it. On success the file watcher
//! fires and broadcasts the new config to subscribers.

use std::future::Future;
use std::pin::Pin;

/// Persists edits to the on-disk YAML config. Implementations are
/// responsible for: serializing concurrent writes, deserialize-merging
/// the section into the full config, running cross-feature validation,
/// writing the file atomically, and notifying reload subscribers.
///
/// Single-section vs whole-file: prefer [`Self::write_section`] from
/// feature crates so unrelated sections stay untouched even on partial
/// writes. Use [`Self::write_all`] for the `PUT /admin/config` endpoint
/// where the whole file is replaced atomically.
pub trait ConfigPersister: Send + Sync {
    fn write_section<'a>(
        &'a self,
        section: &'a str,
        value: serde_yaml::Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), ConfigPersistError>> + Send + 'a>>;

    fn write_all<'a>(
        &'a self,
        value: serde_yaml::Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), ConfigPersistError>> + Send + 'a>>;
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigPersistError {
    /// Cross-feature validation rejected the new config (unknown
    /// provider, dangling judge reference, etc.). The on-disk file is
    /// unchanged; the caller should surface the message verbatim to the
    /// user.
    #[error("invalid config: {0}")]
    Invalid(String),
    /// Filesystem or I/O failure while reading or writing the YAML.
    #[error("config I/O failed: {0}")]
    Io(String),
    /// Serde could not parse the supplied value as YAML or deserialize
    /// the merged result into a `Config` (typed shape mismatch).
    #[error("config parse failed: {0}")]
    Parse(String),
}
