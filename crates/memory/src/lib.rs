mod config;
mod embedder;
mod error;
mod store;
mod types;

pub use config::{BackendConfig, EmbedderConfig, ExtractorConfig, MemoryConfig};
pub use coulisse_core::{Message, MessageId, Role, Score, ScoreId, UserId};
pub use embedder::{BundledEmbedder, HashEmbedder};
pub use error::{ConfigError, EmbedError, MemoryError};
pub use store::{AgentScoreSummary, AssembledContext, Store, UserMemory, UserSummary};
pub use types::{
    Memory, MemoryId, MemoryKind, StoredMessage, StoredToolCall, TokenCount, ToolCallId,
    ToolCallInvocation, ToolCallKind,
};
