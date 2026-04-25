use std::time::{SystemTime, UNIX_EPOCH};

use coulisse_core::{ToolCallKind, TurnId, UserId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// One tool invocation recorded during a turn. Each turn that used tools
/// produces zero or more `ToolCall` rows linked to the `TurnId` that
/// produced them, in the order rig fired them (`ordinal`).
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

impl ToolCall {
    /// Build a fresh `ToolCall` for the given invocation. `id` and
    /// `created_at` are auto-filled; callers pass in only the domain
    /// data grouped into one `ToolCallInvocation` struct so adding a
    /// field in the future doesn't break every call site.
    pub fn new(invocation: ToolCallInvocation) -> Self {
        Self {
            args: invocation.args,
            created_at: now_secs(),
            error: invocation.error,
            id: ToolCallId::new(),
            kind: invocation.kind,
            ordinal: invocation.ordinal,
            result: invocation.result,
            tool_name: invocation.tool_name,
            turn_id: invocation.turn_id,
            user_id: invocation.user_id,
        }
    }
}

/// The domain data needed to create a `ToolCall`, bundled together
/// so the constructor stays readable.
#[derive(Clone, Debug)]
pub struct ToolCallInvocation {
    pub args: String,
    pub error: Option<String>,
    pub kind: ToolCallKind,
    pub ordinal: u32,
    pub result: Option<String>,
    pub tool_name: String,
    pub turn_id: TurnId,
    pub user_id: UserId,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
