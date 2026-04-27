use coulisse_core::MessageId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identity for one synthetic-conversation run. Returned from
/// `Smoke::start_run` and used to navigate to the run viewer in the
/// admin UI.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RunId(pub Uuid);

impl RunId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

/// Lifecycle state for a smoke run. Transitions are linear: `Running` →
/// (`Completed` | `Failed`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Completed,
    Failed,
    Running,
}

impl RunStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Running => "running",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "running" => Some(Self::Running),
            _ => None,
        }
    }
}

/// Which side of the synthetic conversation produced a message. The
/// runner records both sides so the run viewer shows the exchange in
/// full.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnRole {
    Assistant,
    Persona,
}

impl TurnRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::Persona => "persona",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "assistant" => Some(Self::Assistant),
            "persona" => Some(Self::Persona),
            _ => None,
        }
    }
}

/// One row in `smoke_messages`. `message_id` is set on assistant turns
/// and joins to `judges.scores.message_id`; persona turns leave it
/// `None`.
#[derive(Clone, Debug)]
pub struct StoredMessage {
    pub content: String,
    pub message_id: Option<MessageId>,
    pub role: TurnRole,
    pub run_id: RunId,
    pub turn_index: u32,
}

/// One row in `smoke_runs`. `agent_resolved` and `experiment` are
/// populated lazily — they're only known after the experiment router
/// has picked a variant.
#[derive(Clone, Debug)]
pub struct StoredRun {
    pub agent_resolved: Option<String>,
    pub ended_at: Option<u64>,
    pub error: Option<String>,
    pub experiment: Option<String>,
    pub id: RunId,
    pub started_at: u64,
    pub status: RunStatus,
    pub test_name: String,
    pub total_turns: u32,
}
