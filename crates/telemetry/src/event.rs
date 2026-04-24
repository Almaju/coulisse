use std::time::{SystemTime, UNIX_EPOCH};

use memory::UserId;
use serde::{Deserialize, Serialize};

use crate::id::{EventId, TurnId};

/// A single telemetry row. All events emitted while handling one HTTP
/// request share the same `correlation_id`. `parent_id` nests an event
/// under the span that triggered it (e.g. a `ToolCall` emitted inside a
/// `SubagentCall` has the subagent event as its parent).
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
    /// Kind-specific JSON payload. Shape is defined per variant in
    /// `EventKind::payload_example` docstrings. Kept as `serde_json::Value`
    /// so new kinds don't require a schema change.
    pub payload: serde_json::Value,
    pub user_id: UserId,
}

impl Event {
    /// Build a new event with `created_at` set to now and a fresh id.
    /// Fields the caller typically needs to set explicitly are required
    /// arguments; `duration_ms` defaults to `None` and can be overridden.
    pub fn new(
        correlation_id: TurnId,
        user_id: UserId,
        parent_id: Option<EventId>,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            correlation_id,
            created_at: now_millis(),
            duration_ms: None,
            id: EventId::new(),
            kind,
            parent_id,
            payload,
            user_id,
        }
    }

    /// Replace the auto-generated event id. Used by callers that need to
    /// mint the id in advance so children can reference it as their parent
    /// before the span finishes.
    pub fn with_id(mut self, id: EventId) -> Self {
        self.id = id;
        self
    }

    pub fn with_duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }
}

/// What kind of thing happened. Kept deliberately small — Phase 1 covers
/// the blind spot that let hallucinated tool failures hide; richer kinds
/// (per-step latency snapshots, extractor runs, judge scores) land in
/// later phases as they migrate out of `memory` into this crate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Turn arrived from the client. Emitted before any LLM work. Payload:
    /// `{ "agent": "...", "model": "...", "user_message": "..." }`.
    TurnStart,
    /// Turn completed. Payload: `{ "assistant_text": "...", "usage": {...} }`.
    /// `duration_ms` set to total turn time.
    TurnFinish,
    /// Single LLM provider call completed. Payload:
    /// `{ "provider": "...", "model": "...", "prompt": "...", "response": "...", "usage": {...}, "error"?: "..." }`.
    /// `duration_ms` set to the provider round-trip time.
    LlmCall,
    /// Tool invocation completed (MCP or subagent). Payload:
    /// `{ "tool_name": "...", "kind": "mcp"|"subagent", "args": "...", "result"?: "...", "error"?: "..." }`.
    /// `duration_ms` set to tool execution time.
    ToolCall,
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
