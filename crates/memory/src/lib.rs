pub mod admin;
mod config;
mod embedder;
mod error;
mod extractor;
mod store;
mod types;

pub use config::{
    BackendConfig, EmbedderConfig, EmbedderYaml, ExtractorConfig, MemoryConfig, MemoryYaml,
    ProviderModel, UserStateConfig, UserStateYaml, default_dedup_threshold,
    default_extractor_max_facts, default_hash_dims, default_openai_embedding_model,
    default_recall_k, default_sqlite_path, default_voyage_model,
};
pub use coulisse_core::{Message, MessageId, Role, UserId};
pub use embedder::{BundledEmbedder, HashEmbedder};
pub use error::{ConfigError, EmbedError, MemoryError};
pub use extractor::{ExtractInputs, Extractor};
pub use sqlx::SqlitePool;
pub use store::{
    AppendMessage, AssembledContext, ContextRequest, ConversationSummary, RecallQuery,
    RememberIfNovel, RememberInput, Store, StoreInputs, UserMemory, UserSummary, open_pool,
};
pub use types::{
    Memory, MemoryId, MemoryKind, NewMemory, NewStoredMessage, StoredMessage, TokenCount,
};
