use std::ops::{Add, AddAssign};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MemoryId(pub Uuid);

impl MemoryId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MemoryId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MessageId(pub Uuid);

impl MessageId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ScoreId(pub Uuid);

impl ScoreId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ScoreId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ToolCallId(pub Uuid);

impl ToolCallId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ToolCallId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct UserId(pub Uuid);

impl UserId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parse `s` as a UUID if well-formed; otherwise derive a stable UUID from it.
    /// Accepts arbitrary caller-supplied strings without losing partitioning guarantees.
    pub fn from_string(s: &str) -> Self {
        match Uuid::parse_str(s) {
            Ok(uuid) => Self(uuid),
            Err(_) => Self(Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())),
        }
    }
}

impl Default for UserId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for UserId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TokenCount(pub u32);

impl TokenCount {
    /// Rough approximation: ~4 characters per token. Swap for tiktoken when accuracy matters.
    pub fn estimate(text: &str) -> Self {
        let chars = text.chars().count() as u32;
        Self(chars / 4 + 1)
    }

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Assistant,
    System,
    User,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Message {
    pub content: String,
    pub role: Role,
}

impl Message {
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            role: Role::Assistant,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            role: Role::System,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            role: Role::User,
        }
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
    pub fn new(user_id: UserId, role: Role, content: String) -> Self {
        Self::new_with_id(user_id, role, content, MessageId::new())
    }

    /// Build a `StoredMessage` with a caller-supplied id. Used by the chat
    /// handler so the assistant message's id can be generated before the
    /// prompter runs and reused as the telemetry turn correlation id.
    pub fn new_with_id(user_id: UserId, role: Role, content: String, id: MessageId) -> Self {
        let token_count = TokenCount::estimate(&content);
        Self {
            created_at: now_secs(),
            id,
            role,
            token_count,
            user_id,
            content,
        }
    }

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
    pub fn new(user_id: UserId, kind: MemoryKind, content: String, embedding: Vec<f32>) -> Self {
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

/// Single criterion evaluation attached to an assistant message by an LLM judge.
/// Each rubric on a judge produces one `Score` per scored turn; averages and
/// trends are computed at read time (admin views), not aggregated here.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Score {
    pub created_at: u64,
    pub criterion: String,
    pub id: ScoreId,
    pub judge_model: String,
    pub judge_name: String,
    pub message_id: MessageId,
    pub reasoning: String,
    pub score: f32,
    pub user_id: UserId,
}

impl Score {
    pub fn new(
        user_id: UserId,
        message_id: MessageId,
        judge_name: String,
        judge_model: String,
        criterion: String,
        score: f32,
        reasoning: String,
    ) -> Self {
        Self {
            created_at: now_secs(),
            criterion,
            id: ScoreId::new(),
            judge_model,
            judge_name,
            message_id,
            reasoning,
            score,
            user_id,
        }
    }
}

/// Whether a tool invocation was serviced by an MCP server or by another
/// agent acting as a tool (subagent). The distinction matters in the admin
/// UI where subagent calls carry different semantics (nested conversation,
/// own token usage) from MCP calls (pure function invocation).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallKind {
    Mcp,
    Subagent,
}

/// A single tool invocation that happened during an assistant turn. Each
/// turn that used tools produces zero or more `StoredToolCall` rows linked
/// to the assistant message that was eventually returned to the user, in
/// the order they fired (`ordinal`).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredToolCall {
    pub args: String,
    pub created_at: u64,
    pub error: Option<String>,
    pub id: ToolCallId,
    pub kind: ToolCallKind,
    pub message_id: MessageId,
    pub ordinal: u32,
    pub result: Option<String>,
    pub tool_name: String,
    pub user_id: UserId,
}

impl StoredToolCall {
    /// Build a fresh `StoredToolCall` for the given invocation. `id` and
    /// `created_at` are auto-filled; callers pass in only the domain data
    /// grouped into one `ToolCallInvocation` struct (rather than a long
    /// argument list) so adding a field in the future doesn't break
    /// every call site.
    pub fn new(invocation: ToolCallInvocation) -> Self {
        Self {
            args: invocation.args,
            created_at: now_secs(),
            error: invocation.error,
            id: ToolCallId::new(),
            kind: invocation.kind,
            message_id: invocation.message_id,
            ordinal: invocation.ordinal,
            result: invocation.result,
            tool_name: invocation.tool_name,
            user_id: invocation.user_id,
        }
    }
}

/// The domain data needed to create a `StoredToolCall`, bundled together
/// so the constructor stays readable.
#[derive(Clone, Debug)]
pub struct ToolCallInvocation {
    pub args: String,
    pub error: Option<String>,
    pub kind: ToolCallKind,
    pub message_id: MessageId,
    pub ordinal: u32,
    pub result: Option<String>,
    pub tool_name: String,
    pub user_id: UserId,
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
