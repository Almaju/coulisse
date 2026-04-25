mod config;
mod embedder;
mod error;
mod extractor;
mod store;
mod types;

pub use config::{BackendConfig, EmbedderConfig, ExtractorConfig, MemoryConfig};
pub use coulisse_core::{Message, MessageId, Role, UserId};
pub use embedder::{BundledEmbedder, HashEmbedder};
pub use error::{ConfigError, EmbedError, MemoryError};
pub use extractor::Extractor;
pub use store::{AssembledContext, Store, UserMemory, UserSummary, open_pool};
pub use types::{Memory, MemoryId, MemoryKind, StoredMessage, TokenCount};
