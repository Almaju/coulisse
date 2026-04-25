use coulisse_core::{ToolCallKind, TurnId, UserId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ToolCallId(pub Uuid);

impl Default for ToolCallId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

/// One tool invocation recorded during a turn. Read-only view onto rows
/// the `SqliteLayer` writes from `tool_call` tracing spans. Anchored on
/// `turn_id`, in `ordinal` order within a turn.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub args: String,
    pub created_at: u64,
    pub error: Option<String>,
    pub id: ToolCallId,
    pub kind: ToolCallKind,
    pub ordinal: u32,
    pub result: Option<String>,
    pub tool_name: String,
    pub turn_id: TurnId,
    pub user_id: UserId,
}
