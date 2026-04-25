//! Cross-crate primitives shared by every feature crate.
//!
//! This crate stays small on purpose. It holds domain types
//! (`UserId`, `TurnId`, `Message`, `Role`, `AgentScoreSummary`) and
//! tiny cross-cutting traits (`OneShotPrompt`, `ScoreLookup`) that
//! sit at feature-crate boundaries. If a type lives in only one
//! feature, keep it there.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identity for a turn — the public-visible correlation id shared by
/// every event emitted while processing one user-facing request, including
/// nested subagent calls.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TurnId(pub Uuid);

impl TurnId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TurnId {
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Assistant,
    System,
    User,
}

/// Whether a tool invocation was serviced by an MCP server or by another
/// agent acting as a tool (subagent). The distinction matters in the
/// studio UI where subagent calls carry different semantics (nested
/// conversation, own token usage) from MCP calls (pure function
/// invocation). Lives in core because it's emitted on the streaming
/// path (`backends`) and persisted on the observability path
/// (`telemetry`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallKind {
    Mcp,
    Subagent,
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

/// Per-agent score summary aggregated over a time window. One row per
/// agent that has any score in the window; absence means "not enough
/// data". Produced by `judge` (the score authority) and consumed by
/// routing logic in `agents`/`experiments`. Lives in core only because
/// it sits at the trait boundary (`ScoreLookup`); the score storage
/// itself lives in the `judge` crate.
#[derive(Clone, Debug)]
pub struct AgentScoreSummary {
    pub agent_name: String,
    pub mean: f32,
    pub samples: u32,
}

/// Read-only view onto judge score aggregates. Implemented by whichever
/// crate owns the score storage (currently `judge`); consumed by feature
/// crates that need to read scores at runtime — e.g. `agents` for
/// bandit-strategy variant selection during subagent dispatch — without
/// taking a hard dep on the score-storage crate.
pub trait ScoreLookup: Send + Sync {
    fn mean_scores_by_agent<'a>(
        &'a self,
        judge: &'a str,
        criterion: &'a str,
        since: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<AgentScoreSummary>, ScoreLookupError>> + Send + 'a>>;
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ScoreLookupError(pub String);

impl ScoreLookupError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// Single-shot prompt against a named provider/model. Used by features that
/// need to call an LLM out-of-band from the main agent flow — e.g. memory
/// fact extraction, judge scoring. Implemented by whatever crate owns the
/// LLM clients (`agents::RigAgents`); consumed by feature crates so they
/// don't need to depend on `agents` directly.
pub trait OneShotPrompt: Send + Sync {
    fn one_shot<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        preamble: &'a str,
        user_text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, OneShotError>> + Send + 'a>>;
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OneShotError(pub String);

impl OneShotError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}
