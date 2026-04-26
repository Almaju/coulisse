use std::path::PathBuf;

use thiserror::Error;

/// Errors surfaced when constructing a `Store` or resolving config. These
/// are startup-time failures — bad paths, unknown models, missing keys.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("embedder provider '{provider}' client init failed: {message}")]
    ClientInit { provider: String, message: String },
    #[error("failed to create database directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("schema migration failed: {0}")]
    Migrate(#[from] coulisse_core::migrate::MigrateError),
    #[error(
        "embedder provider '{provider}' requires an api_key (set memory.embedder.api_key or providers.{provider}.api_key)"
    )]
    MissingApiKey { provider: String },
    #[error("embedder provider '{provider}' does not support model '{model}'")]
    UnknownModel { model: String, provider: String },
}

/// Errors surfaced during normal request handling. All are recoverable from
/// the caller's perspective — they translate to HTTP errors upstream.
#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("vector dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },
    #[error("embedding failed: {0}")]
    Embed(#[from] EmbedError),
    #[error("no messages in conversation")]
    EmptyConversation,
    #[error("stored data corrupted: {0}")]
    RowDecode(String),
}

#[derive(Debug, Error)]
#[error("embedder error: {message}")]
pub struct EmbedError {
    pub message: String,
}

impl EmbedError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<rig::embeddings::EmbeddingError> for EmbedError {
    fn from(err: rig::embeddings::EmbeddingError) -> Self {
        Self::new(err.to_string())
    }
}
