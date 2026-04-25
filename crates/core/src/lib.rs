//! Cross-crate primitives shared by every feature crate.
//!
//! This crate stays small on purpose. It holds domain types (`UserId`,
//! `TurnId`, `Message`, `Role`, `Score`) that cross feature boundaries.
//! Feature crates depend on `coulisse-core`; they never depend on each
//! other. If a type lives in only one feature, keep it there.

use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Single criterion evaluation attached to an assistant message by an LLM judge.
/// Each rubric on a judge produces one `Score` per scored turn; averages and
/// trends are computed at read time (studio views), not aggregated here.
///
/// `agent_name` is the agent (or experiment variant) whose reply was
/// scored — populated since experiments shipped so per-variant
/// aggregation flows through the same table without a join.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Score {
    pub agent_name: String,
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

/// Per-agent score summary aggregated over a time window. One row per
/// agent that has any score in the window; absence means "not enough
/// data". Produced by storage queries (e.g. `memory::Store`) and consumed
/// by routing logic (e.g. `experiments`).
#[derive(Clone, Debug)]
pub struct AgentScoreSummary {
    pub agent_name: String,
    pub mean: f32,
    pub samples: u32,
}

impl Score {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        user_id: UserId,
        message_id: MessageId,
        agent_name: String,
        judge_name: String,
        judge_model: String,
        criterion: String,
        score: f32,
        reasoning: String,
    ) -> Self {
        Self {
            agent_name,
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Single-shot prompt against a named provider/model. Used by features that
/// need to call an LLM out-of-band from the main agent flow — e.g. memory
/// fact extraction, judge scoring. Implemented by whatever crate owns the
/// LLM clients (currently `prompter`); consumed by feature crates so they
/// don't need to depend on `prompter` directly.
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
