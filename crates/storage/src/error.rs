use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("backend I/O: {0}")]
    Backend(String),
    #[error("file too large: {size} bytes, limit is {limit} bytes")]
    FileTooLarge { limit: u64, size: u64 },
    #[error("migration error: {0}")]
    Migrate(String),
    #[error("file not found: {0}")]
    NotFound(String),
    #[error("quota exceeded: stored {stored} bytes, limit {limit} bytes")]
    QuotaExceeded { limit: u64, stored: u64 },
    #[error("content type not allowed: {0}")]
    UnsupportedContentType(String),
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
}

impl StorageError {
    pub fn backend(msg: impl Into<String>) -> Self {
        Self::Backend(msg.into())
    }
}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        Self::Backend(e.to_string())
    }
}
