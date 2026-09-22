use std::ops::{Add, AddAssign};

use coulisse_core::{Message, MessageId, Role, UserId, now_secs};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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

impl std::str::FromStr for MemoryId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TokenCount(pub u32);

impl TokenCount {
    /// Total context budget (memories + history) used when the resolved
    /// config does not say otherwise.
    pub const DEFAULT_CONTEXT_BUDGET: Self = Self(8_000);

    /// Rough approximation: ~4 characters per token. Swap for tiktoken when accuracy matters.
    #[must_use]
    pub fn estimate(text: &str) -> Self {
        let chars = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        Self(chars / 4 + 1)
    }

    /// Keep the leading memories whose estimated cost fits in this budget,
    /// in the order they were recalled.
    #[must_use]
    pub fn fit_memories(self, recalled: Vec<Memory>) -> Vec<Memory> {
        let mut used = Self(0);
        let mut out = Vec::new();
        for m in recalled {
            let cost = Self::estimate(&m.content);
            if used + cost > self {
                break;
            }
            used += cost;
            out.push(m);
        }
        out
    }

    /// Keep the most recent messages that fit in this budget, returned in
    /// chronological order.
    #[must_use]
    pub fn fit_messages(self, messages: &[StoredMessage]) -> Vec<Message> {
        let mut used = Self(0);
        let mut taken: Vec<&StoredMessage> = Vec::new();
        for m in messages.iter().rev() {
            if used + m.token_count > self {
                break;
            }
            used += m.token_count;
            taken.push(m);
        }
        taken.reverse();
        taken.iter().map(|m| m.as_message()).collect()
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

impl StoredMessage {
    #[must_use]
    pub fn new(user_id: UserId, role: Role, content: String) -> Self {
        Self::new_with_id(user_id, role, content, MessageId::new())
    }

    /// Build a `StoredMessage` with a caller-supplied id. Used by the chat
    /// handler so the assistant message's id can be generated before the
    /// prompter runs and reused as the telemetry turn correlation id.
    #[must_use]
    pub fn new_with_id(user_id: UserId, role: Role, content: String, id: MessageId) -> Self {
        let token_count = TokenCount::estimate(&content);
        Self {
            content,
            created_at: now_secs(),
            id,
            role,
            token_count,
            user_id,
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
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

impl Memory {
    #[must_use]
    pub fn new(user_id: UserId, kind: MemoryKind, content: String, embedding: Vec<f32>) -> Self {
        Self {
            content,
            created_at: now_secs(),
            embedding,
            id: MemoryId::new(),
            kind,
            user_id,
        }
    }
}
