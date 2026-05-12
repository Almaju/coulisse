use std::ops::{Add, AddAssign};

use coulisse_core::{Message, MessageId, Role, UserId, now_secs};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MemoryId(pub Uuid);

impl MemoryId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MemoryId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TokenCount(pub u32);

impl TokenCount {
    /// Rough approximation: ~4 characters per token. Swap for tiktoken when accuracy matters.
    #[must_use]
    pub fn estimate(text: &str) -> Self {
        let chars = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        Self(chars / 4 + 1)
    }

    #[must_use]
    pub fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl Add for TokenCount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl AddAssign for TokenCount {
    fn add_assign(&mut self, rhs: Self) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredMessage {
    pub content: String,
    pub created_at: u64,
    pub id: MessageId,
    pub role: Role,
    pub token_count: TokenCount,
    pub user_id: UserId,
}

/// Inputs to [`StoredMessage::new`]. When `id` is `None` the constructor
/// mints a fresh `MessageId`; pass `Some` so the chat handler can reuse
/// the assistant message id as the telemetry turn correlation id.
pub struct NewStoredMessage {
    pub content: String,
    pub id: Option<MessageId>,
    pub role: Role,
    pub user_id: UserId,
}

impl StoredMessage {
    #[must_use]
    pub fn new(input: NewStoredMessage) -> Self {
        let NewStoredMessage {
            content,
            id,
            role,
            user_id,
        } = input;
        let token_count = TokenCount::estimate(&content);
        Self {
            created_at: now_secs(),
            id: id.unwrap_or_else(MessageId::new),
            role,
            token_count,
            user_id,
            content,
        }
    }

    #[must_use]
    pub fn as_message(&self) -> Message {
        Message {
            content: self.content.clone(),
            role: self.role,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    Fact,
    Preference,
}

impl MemoryKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Preference => "preference",
        }
    }
}

impl std::str::FromStr for MemoryKind {
    type Err = UnknownMemoryKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fact" => Ok(Self::Fact),
            "preference" => Ok(Self::Preference),
            other => Err(UnknownMemoryKind(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown memory kind '{0}'")]
pub struct UnknownMemoryKind(pub String);

#[derive(Clone, Debug)]
pub struct Memory {
    pub content: String,
    pub created_at: u64,
    pub embedding: Vec<f32>,
    pub id: MemoryId,
    pub kind: MemoryKind,
    pub user_id: UserId,
}

/// Inputs to [`Memory::new`]: the owning user, the memory kind, the
/// text content, and its precomputed embedding.
pub struct NewMemory {
    pub content: String,
    pub embedding: Vec<f32>,
    pub kind: MemoryKind,
    pub user_id: UserId,
}

impl Memory {
    #[must_use]
    pub fn new(input: NewMemory) -> Self {
        let NewMemory {
            content,
            embedding,
            kind,
            user_id,
        } = input;
        Self {
            created_at: now_secs(),
            embedding,
            id: MemoryId::new(),
            kind,
            user_id,
            content,
        }
    }
}
