use coulisse_core::{TurnId, UserId};
use serde::{Deserialize, Serialize};

use crate::id::EventId;

/// One row read back from the `events` table. All events emitted while
/// handling one HTTP request share the same `correlation_id` (the
/// `TurnId`); `parent_id` nests an event under the span that triggered it
/// (e.g. a `ToolCall` emitted inside a subagent has the subagent event as
/// its parent). The struct is read-only — writes happen exclusively via
/// the `tracing` crate's `tool_call` / `turn` / `llm_call` spans, which
/// `SqliteLayer` mirrors into this table.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Event {
    pub correlation_id: TurnId,
    pub created_at: u64,
    /// How long the spanned work took, in milliseconds. `None` for point
    /// events (e.g. `TurnStart`) that do not represent a span.
    pub duration_ms: Option<u64>,
    pub id: EventId,
    pub kind: EventKind,
    pub parent_id: Option<EventId>,
    /// Kind-specific JSON payload. Shape is defined per variant:
    ///   - `TurnStart`:  `{ "agent": "...", "experiment"?: "...", "user_message": "..." }`
    ///   - `TurnFinish`: `{ "assistant_text": "...", "usage": {...} }`
    ///   - `LlmCall`:    `{ "provider": "...", "model": "...", "prompt": "...", "response": "...", "usage": {...}, "error"?: "..." }`
    ///   - `ToolCall`:   `{ "tool_name": "...", "kind": "mcp"|"subagent", "args": "...", "result"?: "...", "error"?: "..." }`
    pub payload: serde_json::Value,
    pub user_id: UserId,
}

/// What kind of thing happened. The studio renders the `events` tree
/// keyed on this enum; `SqliteLayer` writes one of these for each
/// recorded `tracing` span.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Single LLM provider call completed.
    LlmCall,
    /// Tool invocation completed (MCP or subagent).
    ToolCall,
    /// Turn completed.
    TurnFinish,
    /// Turn arrived from the client.
    TurnStart,
}
