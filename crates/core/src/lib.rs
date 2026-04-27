//! Cross-crate primitives shared by every feature crate.
//!
//! This crate stays small on purpose. It holds domain types
//! (`UserId`, `TurnId`, `Message`, `Role`, `AgentScoreSummary`),
//! tiny cross-cutting traits (`OneShotPrompt`, `ScoreLookup`,
//! `ConfigPersister`), and a handful of HTTP utilities every feature
//! crate's admin router needs to share — content negotiation,
//! either-form-or-json body parsing — so the studio surface stays a
//! thin representation of the API rather than a parallel codebase.

mod config_store;
pub mod migrate;
mod web;

use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

pub use config_store::{ConfigPersistError, ConfigPersister};
pub use web::{BodyRejection, EitherFormOrJson, ResponseFormat, redirect_to};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Seconds since the Unix epoch. Saturates to 0 if the system clock is
/// before 1970 (impossible in normal operation, but the call is
/// infallible to keep call sites readable).
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Convert a u64 timestamp/count to i64 for `SQLite` `INTEGER` binds.
/// Saturates rather than panicking — the only inputs that would overflow
/// are timestamps several billion years from now, or token counts that
/// would never fit through any real provider.
#[must_use]
pub fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Convert a signed integer read from `SQLite` back to u64. Negative
/// values (which should never appear because we only store from u64
/// originals) are clamped to 0.
#[must_use]
pub fn i64_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Saturating cast for counts read from `SQLite` as i64 but exposed
/// to callers as u32 (judge sample counts, turn indices, etc.).
/// Negatives clamp to 0; values above `u32::MAX` clamp to `u32::MAX`.
#[must_use]
pub fn i64_to_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// Stable identity for a turn — the public-visible correlation id shared by
/// every event emitted while processing one user-facing request, including
/// nested subagent calls.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TurnId(pub Uuid);

impl TurnId {
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parse `s` as a UUID if well-formed; otherwise derive a stable UUID from it.
    /// Accepts arbitrary caller-supplied strings without losing partitioning guarantees.
    #[must_use]
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

impl Role {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::User => "user",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = UnknownRole;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            other => Err(UnknownRole(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown role '{0}'")]
pub struct UnknownRole(pub String);

/// Whether a tool invocation was serviced by an MCP server or by another
/// agent acting as a tool (subagent). The distinction matters in the
/// studio UI where subagent calls carry different semantics (nested
/// conversation, own token usage) from MCP calls (pure function
/// invocation). Lives in core because it's emitted on the streaming
/// path (`providers`) and persisted on the observability path
/// (`telemetry`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallKind {
    Mcp,
    Subagent,
}

impl ToolCallKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Subagent => "subagent",
        }
    }
}

impl std::str::FromStr for ToolCallKind {
    type Err = UnknownToolCallKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mcp" => Ok(Self::Mcp),
            "subagent" => Ok(Self::Subagent),
            other => Err(UnknownToolCallKind(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown tool call kind '{0}'")]
pub struct UnknownToolCallKind(pub String);

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
/// data". Produced by `judges` (the score authority) and consumed by
/// routing logic in `agents`/`experiments`. Lives in core only because
/// it sits at the trait boundary (`ScoreLookup`); the score storage
/// itself lives in the `judges` crate.
#[derive(Clone, Debug)]
pub struct AgentScoreSummary {
    pub agent_name: String,
    pub mean: f32,
    pub samples: u32,
}

/// Read-only view onto judge score aggregates. Implemented by whichever
/// crate owns the score storage (currently `judges`); consumed by feature
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

/// Map an addressable name (agent or experiment) to a concrete agent name
/// for a given user. Implemented by the `experiments` crate;
/// consumed by `agents` so the runtime can dispatch subagent calls
/// without taking a hard dep on `experiments`. For non-experiment names,
/// `resolve` returns the input unchanged.
pub trait AgentResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
        user_id: UserId,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>>;

    /// Tool description for an addressable name when used as a subagent.
    /// `None` means the name isn't a known experiment — agents falls
    /// back to its own per-agent purpose lookup.
    fn purpose(&self, name: &str) -> Option<String>;
}
