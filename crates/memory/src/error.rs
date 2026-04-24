use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("vector dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },
    #[error("embedding failed: {0}")]
    Embed(#[from] EmbedError),
    #[error("no messages in conversation")]
    EmptyConversation,
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
