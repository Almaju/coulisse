use std::path::PathBuf;
use std::pin::Pin;

use tokio::fs;
use tokio::io::AsyncWriteExt as _;

use crate::error::StorageError;

/// Blob storage backend abstraction. Object-safe so backends can be
/// tested independently without the full `Store`.
pub trait Backend: Send + Sync {
    fn put<'a>(
        &'a self,
        key: &'a str,
        data: &'a [u8],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), StorageError>> + Send + 'a>>;

    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, StorageError>> + Send + 'a>>;

    fn delete<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), StorageError>> + Send + 'a>>;

    /// List all blob keys present in physical storage. Used at boot to
    /// reconcile the SQLite index against the filesystem. Implementations
    /// that can't enumerate (S3) return `Ok(vec![])`.
    fn list_keys<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<String>, StorageError>> + Send + 'a>>;
}

/// Concrete enum over the supported blob backends. Avoids boxing futures
/// on the hot path while still giving `Store` a single field type.
pub enum BlobBackend {
    Fs(FsBackend),
    #[cfg(feature = "s3")]
    S3(crate::s3::S3Backend),
}

impl BlobBackend {
    pub async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        match self {
            Self::Fs(b) => b.put(key, bytes).await,
            #[cfg(feature = "s3")]
            Self::S3(b) => b.put(key, bytes).await,
        }
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        match self {
            Self::Fs(b) => b.get(key).await,
            #[cfg(feature = "s3")]
            Self::S3(b) => b.get(key).await,
        }
    }

    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        match self {
            Self::Fs(b) => b.delete(key).await,
            #[cfg(feature = "s3")]
            Self::S3(b) => b.delete(key).await,
        }
    }

    /// Returns physical keys for fs backends (used at boot to reconcile the
    /// SQLite index). Returns an empty vec for S3 (lazy reconciliation).
    pub async fn list_keys(&self) -> Result<Vec<String>, StorageError> {
        match self {
            Self::Fs(b) => b.list_keys().await,
            #[cfg(feature = "s3")]
            Self::S3(_) => Ok(vec![]),
        }
    }
}

/// Filesystem blob backend. Stores one file per blob under `root`.
/// The blob key is a file UUID so directory traversal is impossible.
pub struct FsBackend {
    root: PathBuf,
}

impl FsBackend {
    /// Create or open a backend rooted at `root`. Creates the directory
    /// if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub async fn new(root: impl AsRef<std::path::Path>) -> Result<Self, StorageError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)
            .await
            .map_err(|e| StorageError::backend(format!("create dir {}: {e}", root.display())))?;
        Ok(Self { root })
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}

impl Backend for FsBackend {
    fn put<'a>(
        &'a self,
        key: &'a str,
        data: &'a [u8],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), StorageError>> + Send + 'a>> {
        Box::pin(async move {
            let path = self.path_for(key);
            let mut file = fs::File::create(&path)
                .await
                .map_err(|e| StorageError::backend(format!("create {}: {e}", path.display())))?;
            file.write_all(data)
                .await
                .map_err(|e| StorageError::backend(format!("write {}: {e}", path.display())))?;
            file.flush()
                .await
                .map_err(|e| StorageError::backend(format!("flush {}: {e}", path.display())))?;
            Ok(())
        })
    }

    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, StorageError>> + Send + 'a>> {
        Box::pin(async move {
            let path = self.path_for(key);
            fs::read(&path).await.map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    StorageError::NotFound(key.to_string())
                } else {
                    StorageError::backend(format!("read {}: {e}", path.display()))
                }
            })
        })
    }

    fn delete<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), StorageError>> + Send + 'a>> {
        Box::pin(async move {
            let path = self.path_for(key);
            match fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(StorageError::backend(format!(
                    "delete {}: {e}",
                    path.display()
                ))),
            }
        })
    }

    fn list_keys<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<String>, StorageError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut keys = Vec::new();
            let mut dir = fs::read_dir(&self.root).await.map_err(|e| {
                StorageError::backend(format!("read_dir {}: {e}", self.root.display()))
            })?;
            while let Some(entry) = dir.next_entry().await.map_err(|e| {
                StorageError::backend(format!("dir entry {}: {e}", self.root.display()))
            })? {
                if let Some(name) = entry.file_name().to_str() {
                    keys.push(name.to_string());
                }
            }
            Ok(keys)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FsBackend::new(dir.path()).await.unwrap();

        backend.put("file-123", b"hello").await.unwrap();
        let data = backend.get("file-123").await.unwrap();
        assert_eq!(data, b"hello");

        backend.delete("file-123").await.unwrap();
        let err = backend.get("file-123").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_missing_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FsBackend::new(dir.path()).await.unwrap();
        backend.delete("does-not-exist").await.unwrap();
    }

    #[tokio::test]
    async fn creates_root_dir_if_absent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("deep").join("nested");
        FsBackend::new(root).await.unwrap();
    }

    #[tokio::test]
    async fn list_keys_returns_stored_files() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FsBackend::new(dir.path()).await.unwrap();
        backend.put("key-a", b"1").await.unwrap();
        backend.put("key-b", b"2").await.unwrap();
        let mut keys = backend.list_keys().await.unwrap();
        keys.sort();
        assert_eq!(keys, vec!["key-a", "key-b"]);
    }
}
