use coulisse_core::{MessageId, UserId, now_secs};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One scored exchange — the user/assistant pair plus the identifiers needed
/// to attribute and persist scores. Kept as a single value so `spawn_score`
/// and `run_score` don't drift into 8-arg signatures whenever a new field
/// (turn id, language, etc.) needs to ride along.
#[derive(Clone, Debug)]
pub struct ScoredExchange {
    pub agent_name: String,
    pub assistant_message: String,
    pub message_id: MessageId,
    pub user_id: UserId,
    pub user_message: String,
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
