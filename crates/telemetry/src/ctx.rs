use coulisse_core::{TurnId, UserId};

use crate::id::EventId;

/// Request-scoped telemetry context threaded through the prompter so that
/// nested spans (subagent calls, inner MCP tool calls, LLM provider calls)
/// share a single correlation id and form a parent-linked tree.
///
/// Lightweight by design: carries only ids, never the sink — the sink is a
/// process-level resource held by the `RigAgents`. Clone freely across
/// await points.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub correlation_id: TurnId,
    /// Id of the currently-entered scope, or `None` at the outermost level.
    /// Events emitted with this ctx set their `parent_id` to `parent`.
    pub parent: Option<EventId>,
    pub user_id: UserId,
}

impl Ctx {
    pub fn new(user_id: UserId, correlation_id: TurnId) -> Self {
        Self {
            correlation_id,
            parent: None,
            user_id,
        }
    }

    /// Build a context for a fresh synthetic turn. Useful for tests and for
    /// internal prompter calls (extractor, judge) that aren't user-facing
    /// and don't correlate with a real request.
    pub fn synthetic() -> Self {
        Self::new(UserId::default(), TurnId::new())
    }

    pub fn child(&self, parent: EventId) -> Self {
        Self {
            parent: Some(parent),
            ..*self
        }
    }
}
