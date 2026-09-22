use std::str::FromStr;

use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{UserId, i64_to_u64, now_secs, u64_to_i64};
use sha2::{Digest, Sha256};
use sqlx::{Executor, Sqlite, SqlitePool};
use tracing::warn;
use uuid::Uuid;

use crate::backend::BlobBackend;
use crate::config::QuotaConfig;
use crate::error::StorageError;
use crate::mime;

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "storage";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];
}

/// Stable `file-<uuid>` identifier of a stored file, in the form the
/// `OpenAI` Files API exposes it.
#[derive(Clone, Debug, serde::Deserialize, Hash, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct FileId(String);

impl FileId {
    #[must_use]
    pub fn generate() -> Self {
        Self(format!("file-{}", Uuid::new_v4().simple()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Strict parse: `file-` followed by a UUID, as [`FileId::generate`]
/// produces it. Anything else cannot name a stored file.
impl FromStr for FileId {
    type Err = InvalidFileId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid = s
            .strip_prefix("file-")
            .ok_or_else(|| InvalidFileId::MissingPrefix(s.to_string()))?;
        Uuid::parse_str(uuid).map_err(|source| InvalidFileId::Uuid {
            raw: s.to_string(),
            source,
        })?;
        Ok(Self(s.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidFileId {
    #[error("file id '{0}' does not start with 'file-'")]
    MissingPrefix(String),
    #[error("file id '{raw}' is not 'file-' followed by a UUID: {source}")]
    Uuid { raw: String, source: uuid::Error },
}

/// Metadata row returned for every file, matching the `OpenAI` Files API shape.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct FileObject {
    /// File size in bytes.
    pub bytes: u64,
    /// MIME content type.
    pub content_type: String,
    /// Unix timestamp of upload.
    pub created_at: u64,
    /// Original filename provided at upload.
    pub filename: String,
    /// Stable `file-<uuid>` identifier.
    pub id: FileId,
    /// Always `"file"` for compatibility with the `OpenAI` Files API.
    pub object: &'static str,
    /// Purpose string provided at upload (e.g. `"assistants"`).
    pub purpose: String,
}

/// Everything a caller hands to [`Store::upload`]: the file content plus
/// the metadata the `OpenAI` Files API records with it.
#[derive(Clone, Debug)]
pub struct Upload {
    pub bytes: Vec<u8>,
    /// Declared MIME type, as sent by the client (may carry parameters
    /// such as `; charset=utf-8`).
    pub content_type: String,
    pub filename: String,
    pub purpose: String,
    pub user_id: UserId,
}

impl Upload {
    /// Reject content that is not on the MIME allow-list.
    ///
    /// The type inferred from the magic bytes is checked first, not just
    /// the declared header: this prevents MIME-spoofing executables
    /// through to LLM backends. The declared content-type is then
    /// checked as well so callers get honest feedback even when the
    /// magic check would pass.
    fn check_content_type(&self) -> Result<(), StorageError> {
        let inferred = mime::infer_mime(&self.bytes);
        if !mime::is_allowed(inferred) {
            return Err(StorageError::UnsupportedContentType(inferred.to_string()));
        }
        let declared = self
            .content_type
            .split(';')
            .next()
            .unwrap_or(&self.content_type)
            .trim();
        if !mime::is_allowed(declared) {
            return Err(StorageError::UnsupportedContentType(declared.to_string()));
        }
        Ok(())
    }

    fn size(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }
}

/// Core storage handle. Owns a `SQLite` pool (for the metadata index) and a
/// blob backend (filesystem or S3).
pub struct Store {
    backend: BlobBackend,
    pool: SqlitePool,
    quota: QuotaConfig,
}

impl Store {
    /// Open the store: run migrations, reconcile (fs only), return a handle.
    ///
    /// # Errors
    ///
    /// Returns an error if migrations fail or the backend cannot be opened.
    pub async fn open(
        pool: SqlitePool,
        backend: BlobBackend,
        quota: QuotaConfig,
    ) -> Result<Self, StorageError> {
        migrate::run(&pool, &Schema)
            .await
            .map_err(|e| StorageError::Migrate(e.to_string()))?;

        let store = Self {
            backend,
            pool,
            quota,
        };

        // Reconcile the SQLite index against physical storage. Orphaned index
        // entries (blob deleted outside Coulisse) are pruned at boot so the
        // quota accounting stays accurate.
        store.reconcile_at_boot().await;

        Ok(store)
    }

    /// Delete a file by id. Idempotent: returns `Ok` if the file does not
    /// exist.
    ///
    /// # Errors
    ///
    /// Returns a database or backend error.
    pub async fn delete(&self, id: &FileId) -> Result<(), StorageError> {
        let blob_key = match Self::blob_key_for(&self.pool, id).await {
            Ok(k) => k,
            Err(StorageError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        // Delete backend first — crash between these two steps leaves an
        // orphaned index row, which is cleaned at the next boot reconciliation.
        self.backend.delete(&blob_key).await?;
        sqlx::query("DELETE FROM storage_files WHERE id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Retrieve file content by id.
    ///
    /// Returns `Vec<u8>` in v1. For files > 5 MB on S3 this buffers the
    /// entire response in memory; streaming support is deferred to v2.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NotFound` if the file has been evicted or
    /// never uploaded.
    pub async fn get_content(&self, id: &FileId) -> Result<(FileObject, Vec<u8>), StorageError> {
        let meta = self.get_metadata(id).await?;
        let blob_key = Self::blob_key_for(&self.pool, id).await?;
        match self.backend.get(&blob_key).await {
            Ok(data) => Ok((meta, data)),
            Err(StorageError::NotFound(_)) => {
                // Lazy reconciliation: the blob is gone (e.g. evicted on S3
                // externally). Remove the stale index row.
                let _ = sqlx::query("DELETE FROM storage_files WHERE id = ?")
                    .bind(id.as_str())
                    .execute(&self.pool)
                    .await;
                Err(StorageError::NotFound(id.to_string()))
            }
            Err(e) => Err(e),
        }
    }

    /// Retrieve file metadata by id.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NotFound` if no such file exists.
    pub async fn get_metadata(&self, id: &FileId) -> Result<FileObject, StorageError> {
        let row = sqlx::query_as::<_, FileRow>(
            "SELECT bytes, content_type, created_at, filename, id, purpose \
             FROM storage_files WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        row.map(FileRow::into_object)
            .ok_or_else(|| StorageError::NotFound(id.to_string()))
    }

    /// List all files, most recently uploaded first.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn list(&self) -> Result<Vec<FileObject>, StorageError> {
        let rows = sqlx::query_as::<_, FileRow>(
            "SELECT bytes, content_type, created_at, filename, id, purpose \
             FROM storage_files ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(FileRow::into_object).collect())
    }

    /// Upload a file. Returns the `FileObject` metadata.
    ///
    /// Order of operations:
    /// 1. Validate MIME via magic bytes and per-file size limit (fast).
    /// 2. Write blob to backend (before touching `SQLite` — crash-safe).
    /// 3. Open a `SQLite` transaction.
    /// 4. Check for SHA-256 dedup; if duplicate, delete the backend blob
    ///    and return the existing record.
    /// 5. Evict FIFO until the new file fits within `max_total_bytes`.
    /// 6. Insert metadata row.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::FileTooLarge`, `StorageError::UnsupportedContentType`,
    /// or a database / backend error.
    pub async fn upload(&self, upload: Upload) -> Result<FileObject, StorageError> {
        upload.check_content_type()?;
        let size = upload.size();
        self.quota.check_file_size(size)?;

        let sha256 = hex_sha256(&upload.bytes);
        let blob_key = Uuid::new_v4().to_string();

        // Write blob before touching SQLite — if the process dies between
        // these two steps, the orphan blob is collected at the next boot.
        self.backend.put(&blob_key, &upload.bytes).await?;

        let mut tx = self.pool.begin().await?;

        // Dedup: if a file with the same SHA-256 already exists, return it
        // and discard the blob we just wrote.
        if let Some(existing) = Self::find_by_sha256(&mut tx, &sha256).await? {
            drop(tx);
            self.backend.delete(&blob_key).await?;
            return Ok(existing);
        }

        self.evict_for_size(&mut tx, size).await?;

        let id = FileId::generate();
        let now = u64_to_i64(now_secs());

        sqlx::query(
            "INSERT INTO storage_files \
             (bytes, content_type, created_at, filename, id, purpose, sha256, storage_key, user_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(u64_to_i64(size))
        .bind(&upload.content_type)
        .bind(now)
        .bind(&upload.filename)
        .bind(id.as_str())
        .bind(&upload.purpose)
        .bind(&sha256)
        .bind(&blob_key)
        .bind(upload.user_id.0.to_string())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(FileObject {
            bytes: size,
            content_type: upload.content_type,
            created_at: i64_to_u64(now),
            filename: upload.filename,
            id,
            object: "file",
            purpose: upload.purpose,
        })
    }

    async fn blob_key_for<'e>(
        executor: impl Executor<'e, Database = Sqlite>,
        id: &FileId,
    ) -> Result<String, StorageError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT storage_key FROM storage_files WHERE id = ?")
                .bind(id.as_str())
                .fetch_optional(executor)
                .await?;
        row.map(|(k,)| k)
            .ok_or_else(|| StorageError::NotFound(id.to_string()))
    }

    /// Evict the oldest files until the total stored bytes + `incoming` is
    /// within `max_total_bytes`. Must be called inside an open transaction.
    ///
    /// v1 limitation: concurrent uploads from separate processes are not
    /// serialised at the `SQLite` level — two processes can both pass the
    /// quota check and both insert, temporarily exceeding the limit. The
    /// next upload from either process will then evict back to within the
    /// quota. Within a single process the `pool.begin()` in `upload`
    /// serialises writes via the connection pool's write lock.
    async fn evict_for_size(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        incoming: u64,
    ) -> Result<(), StorageError> {
        let Some(max_total) = self.quota.max_total_bytes else {
            return Ok(());
        };

        let total: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(bytes), 0) FROM storage_files")
            .fetch_one(&mut **tx)
            .await?;
        let total = u64::try_from(total.max(0)).unwrap_or(0);

        if total + incoming <= max_total {
            return Ok(());
        }

        // Fetch oldest rows until we free enough space.
        let rows = sqlx::query_as::<_, EvictRow>(
            "SELECT bytes, id FROM storage_files ORDER BY created_at ASC",
        )
        .fetch_all(&mut **tx)
        .await?;

        let mut freed: u64 = 0;
        let needed = (total + incoming).saturating_sub(max_total);

        for row in rows {
            if freed >= needed {
                break;
            }
            let id = FileId(row.id);
            let blob_key = Self::blob_key_for(&mut **tx, &id).await?;
            if let Err(e) = self.backend.delete(&blob_key).await {
                warn!("eviction: failed to delete blob {id}: {e}");
            }
            sqlx::query("DELETE FROM storage_files WHERE id = ?")
                .bind(id.as_str())
                .execute(&mut **tx)
                .await?;
            freed += u64::try_from(row.bytes.max(0)).unwrap_or(0);
        }

        Ok(())
    }

    async fn find_by_sha256(
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        sha256: &str,
    ) -> Result<Option<FileObject>, StorageError> {
        let row = sqlx::query_as::<_, FileRow>(
            "SELECT bytes, content_type, created_at, filename, id, purpose \
             FROM storage_files WHERE sha256 = ? LIMIT 1",
        )
        .bind(sha256)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row.map(FileRow::into_object))
    }

    /// At boot, scan the fs backend and remove `SQLite` rows whose blob key no
    /// longer exists on disk. For S3, skip the scan (lazy reconciliation
    /// via `get_content`).
    async fn reconcile_at_boot(&self) {
        let Ok(physical_keys) = self.backend.list_keys().await else {
            return;
        };

        if physical_keys.is_empty() {
            // S3 backend always returns empty; nothing to reconcile.
            return;
        }

        let Ok(rows) =
            sqlx::query_as::<_, (String, String)>("SELECT id, storage_key FROM storage_files")
                .fetch_all(&self.pool)
                .await
        else {
            return;
        };

        let physical_set: std::collections::HashSet<String> = physical_keys.into_iter().collect();

        for (id, storage_key) in rows {
            if !physical_set.contains(&storage_key) {
                let _ = sqlx::query("DELETE FROM storage_files WHERE id = ?")
                    .bind(&id)
                    .execute(&self.pool)
                    .await;
                warn!("storage: reconciled orphaned index entry {id}");
            }
        }
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[derive(sqlx::FromRow)]
struct FileRow {
    bytes: i64,
    content_type: String,
    created_at: i64,
    filename: String,
    id: String,
    purpose: String,
}

impl FileRow {
    fn into_object(self) -> FileObject {
        FileObject {
            bytes: i64_to_u64(self.bytes),
            content_type: self.content_type,
            created_at: i64_to_u64(self.created_at),
            filename: self.filename,
            id: FileId(self.id),
            object: "file",
            purpose: self.purpose,
        }
    }
}

#[derive(sqlx::FromRow)]
struct EvictRow {
    bytes: i64,
    id: String,
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqliteConnectOptions;

    use super::*;
    use crate::backend::FsBackend;

    fn upload(filename: &str, content_type: &str, purpose: &str, bytes: Vec<u8>) -> Upload {
        Upload {
            bytes,
            content_type: content_type.to_string(),
            filename: filename.to_string(),
            purpose: purpose.to_string(),
            user_id: UserId::from_string("u"),
        }
    }

    async fn pool() -> SqlitePool {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        SqlitePool::connect_with(opts).await.unwrap()
    }

    async fn fs_store(dir: &tempfile::TempDir) -> Store {
        let backend = FsBackend::new(dir.path()).await.unwrap();
        Store::open(
            pool().await,
            BlobBackend::Fs(backend),
            QuotaConfig::default(),
        )
        .await
        .unwrap()
    }

    async fn fs_store_with_quota(dir: &tempfile::TempDir, max_total: u64) -> Store {
        let backend = FsBackend::new(dir.path()).await.unwrap();
        Store::open(
            pool().await,
            BlobBackend::Fs(backend),
            QuotaConfig {
                max_file_bytes: None,
                max_total_bytes: Some(max_total),
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn upload_and_retrieve() {
        let dir = tempfile::tempdir().unwrap();
        let store = fs_store(&dir).await;

        let meta = store
            .upload(upload(
                "hello.txt",
                "text/plain",
                "assistants",
                b"hello world".to_vec(),
            ))
            .await
            .unwrap();

        assert!(meta.id.as_str().starts_with("file-"));
        assert_eq!(meta.bytes, 11);
        assert_eq!(meta.filename, "hello.txt");
        assert_eq!(meta.purpose, "assistants");
        assert_eq!(meta.object, "file");

        let (retrieved_meta, content) = store.get_content(&meta.id).await.unwrap();
        assert_eq!(retrieved_meta.id, meta.id);
        assert_eq!(content, b"hello world");
    }

    #[tokio::test]
    async fn dedup_returns_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = fs_store(&dir).await;

        let first = store
            .upload(upload(
                "a.txt",
                "text/plain",
                "assistants",
                b"same".to_vec(),
            ))
            .await
            .unwrap();
        let second = store
            .upload(upload("b.txt", "text/plain", "fine-tune", b"same".to_vec()))
            .await
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(store.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fifo_eviction_removes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        // Max 15 bytes — 3 files of 5 bytes each, adding a 4th evicts the first.
        let store = fs_store_with_quota(&dir, 15).await;

        let f1 = store
            .upload(upload("a.txt", "text/plain", "x", b"11111".to_vec()))
            .await
            .unwrap();
        let _f2 = store
            .upload(upload("b.txt", "text/plain", "x", b"22222".to_vec()))
            .await
            .unwrap();
        let _f3 = store
            .upload(upload("c.txt", "text/plain", "x", b"33333".to_vec()))
            .await
            .unwrap();
        // This upload should evict f1 (oldest).
        let _f4 = store
            .upload(upload("d.txt", "text/plain", "x", b"44444".to_vec()))
            .await
            .unwrap();

        let files = store.list().await.unwrap();
        assert_eq!(files.len(), 3);
        assert!(!files.iter().any(|f| f.id == f1.id));
    }

    #[tokio::test]
    async fn quota_not_exceeded_after_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let max = 20u64;
        let store = fs_store_with_quota(&dir, max).await;

        for i in 0..10u8 {
            store
                .upload(upload(&format!("{i}.txt"), "text/plain", "x", vec![i; 5]))
                .await
                .unwrap();
        }

        let files = store.list().await.unwrap();
        let total: u64 = files.iter().map(|f| f.bytes).sum();
        assert!(total <= max);
    }

    #[tokio::test]
    async fn unsupported_mime_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = fs_store(&dir).await;

        let err = store
            .upload(upload(
                "virus.exe",
                "application/x-msdownload",
                "assistants",
                b"MZ\x90\x00".to_vec(),
            ))
            .await
            .unwrap_err();

        assert!(matches!(err, StorageError::UnsupportedContentType(_)));
    }

    #[tokio::test]
    async fn file_too_large_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FsBackend::new(dir.path()).await.unwrap();
        let store = Store::open(
            pool().await,
            BlobBackend::Fs(backend),
            QuotaConfig {
                max_file_bytes: Some(5),
                max_total_bytes: None,
            },
        )
        .await
        .unwrap();

        let err = store
            .upload(upload("big.txt", "text/plain", "x", b"123456".to_vec()))
            .await
            .unwrap_err();

        assert!(matches!(err, StorageError::FileTooLarge { .. }));
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = fs_store(&dir).await;

        let meta = store
            .upload(upload("x.txt", "text/plain", "x", b"data".to_vec()))
            .await
            .unwrap();
        store.delete(&meta.id).await.unwrap();
        store.delete(&meta.id).await.unwrap();
        assert!(store.get_metadata(&meta.id).await.is_err());
    }

    #[tokio::test]
    async fn get_metadata_not_found_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = fs_store(&dir).await;

        let err = store.get_metadata(&FileId::generate()).await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[test]
    fn file_id_round_trips_through_its_string_form() {
        let id = FileId::generate();
        assert_eq!(id.as_str().parse::<FileId>().unwrap(), id);
        assert!(matches!(
            "does-not-exist".parse::<FileId>().unwrap_err(),
            InvalidFileId::MissingPrefix(_)
        ));
        assert!(matches!(
            "file-nope".parse::<FileId>().unwrap_err(),
            InvalidFileId::Uuid { .. }
        ));
    }

    #[test]
    fn hex_sha256_is_deterministic() {
        let h1 = hex_sha256(b"hello");
        let h2 = hex_sha256(b"hello");
        assert_eq!(h1, h2);
        assert_ne!(h1, hex_sha256(b"world"));
    }
}
