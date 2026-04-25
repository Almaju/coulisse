//! Read-only JSON endpoints for the studio UI.
//!
//! These live under `/studio/api/*` and are intentionally minimal: list users,
//! read one user's messages, read one user's long-term memories. Writes and
//! auth are deliberately out of scope — see `docs/src/features/studio-ui.md`.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use memory::{
    Memory, MemoryError, MemoryId, MemoryKind, Role, Score, StoredMessage, StoredToolCall,
    ToolCallKind, UserId, UserSummary,
};
use prompter::Prompter;
use serde::Serialize;
use telemetry::{Event as TelemetryEvent, EventKind, TelemetryError, TurnId};
use uuid::Uuid;

use crate::AppState;

pub fn router<P: Prompter + 'static>() -> Router<Arc<AppState<P>>> {
    Router::new()
        .route("/users", get(list_users::<P>))
        .route("/users/{user_id}/memories", get(user_memories::<P>))
        .route("/users/{user_id}/messages", get(user_messages::<P>))
        .route("/users/{user_id}/scores", get(user_scores::<P>))
        .route(
            "/users/{user_id}/turns/{turn_id}/events",
            get(turn_events::<P>),
        )
}

async fn list_users<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
) -> Result<Json<UsersResponse>, StudioError> {
    let users = state
        .memory
        .list_user_summaries()
        .await?
        .into_iter()
        .map(UserView::from)
        .collect();
    Ok(Json(UsersResponse { users }))
}

async fn user_messages<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
    Path(user_id): Path<String>,
) -> Result<Json<MessagesResponse>, StudioError> {
    use std::collections::HashMap;

    let user_id = parse_user_id(&user_id)?;
    let um = state.memory.for_user(user_id);
    let messages = um.messages().await?;
    let tool_calls = um.tool_calls().await?;

    // Group tool calls by message so the UI can render them inline with
    // the assistant turn they belong to, in fire order.
    let mut by_message: HashMap<String, Vec<ToolCallView>> = HashMap::new();
    for tc in tool_calls {
        by_message
            .entry(tc.message_id.0.to_string())
            .or_default()
            .push(ToolCallView::from(tc));
    }
    for calls in by_message.values_mut() {
        calls.sort_by_key(|t| t.ordinal);
    }

    let messages: Vec<MessageView> = messages
        .into_iter()
        .map(|m| {
            let id = m.id.0.to_string();
            let tool_calls = by_message.remove(&id).unwrap_or_default();
            let mut view = MessageView::from(m);
            view.tool_calls = tool_calls;
            view
        })
        .collect();
    Ok(Json(MessagesResponse { messages }))
}

async fn user_memories<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
    Path(user_id): Path<String>,
) -> Result<Json<MemoriesResponse>, StudioError> {
    let user_id = parse_user_id(&user_id)?;
    let um = state.memory.for_user(user_id);
    let memories = um
        .memories()
        .await?
        .into_iter()
        .map(MemoryView::from)
        .collect();
    Ok(Json(MemoriesResponse { memories }))
}

async fn turn_events<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
    Path((user_id, turn_id)): Path<(String, String)>,
) -> Result<Json<EventsResponse>, StudioError> {
    let user_id = parse_user_id(&user_id)?;
    let turn_id = parse_turn_id(&turn_id)?;
    let events = state
        .telemetry
        .fetch_turn(user_id, turn_id)
        .await?
        .into_iter()
        .map(EventView::from)
        .collect();
    Ok(Json(EventsResponse { events }))
}

async fn user_scores<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
    Path(user_id): Path<String>,
) -> Result<Json<ScoresResponse>, StudioError> {
    let user_id = parse_user_id(&user_id)?;
    let um = state.memory.for_user(user_id);
    let scores: Vec<ScoreView> = um
        .scores()
        .await?
        .into_iter()
        .map(ScoreView::from)
        .collect();
    let averages = average_by_criterion(&scores);
    Ok(Json(ScoresResponse { averages, scores }))
}

/// Mean score per (judge, criterion), recomputed on every request so the
/// UI always sees the latest data without a materialized view.
fn average_by_criterion(scores: &[ScoreView]) -> Vec<CriterionAverage> {
    use std::collections::HashMap;
    let mut buckets: HashMap<(String, String), (f64, u32)> = HashMap::new();
    for s in scores {
        let entry = buckets
            .entry((s.judge_name.clone(), s.criterion.clone()))
            .or_insert((0.0, 0));
        entry.0 += s.score as f64;
        entry.1 += 1;
    }
    let mut out: Vec<CriterionAverage> = buckets
        .into_iter()
        .map(|((judge_name, criterion), (sum, count))| CriterionAverage {
            average: (sum / count as f64) as f32,
            count,
            criterion,
            judge_name,
        })
        .collect();
    out.sort_by(|a, b| {
        a.judge_name
            .cmp(&b.judge_name)
            .then_with(|| a.criterion.cmp(&b.criterion))
    });
    out
}

/// Studio endpoints expect a real UUID in the path — unlike chat requests,
/// there's no sensible way to derive one from arbitrary strings here, since
/// the caller is trying to look up a specific pre-existing record.
fn parse_user_id(raw: &str) -> Result<UserId, StudioError> {
    Uuid::parse_str(raw)
        .map(UserId::from)
        .map_err(|_| StudioError::InvalidUserId)
}

fn parse_turn_id(raw: &str) -> Result<TurnId, StudioError> {
    Uuid::parse_str(raw)
        .map(TurnId)
        .map_err(|_| StudioError::InvalidTurnId)
}

#[derive(Debug)]
enum StudioError {
    InvalidTurnId,
    InvalidUserId,
    Memory(MemoryError),
    Telemetry(TelemetryError),
}

impl From<MemoryError> for StudioError {
    fn from(err: MemoryError) -> Self {
        Self::Memory(err)
    }
}

impl From<TelemetryError> for StudioError {
    fn from(err: TelemetryError) -> Self {
        Self::Telemetry(err)
    }
}

impl IntoResponse for StudioError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidTurnId => (
                StatusCode::BAD_REQUEST,
                "turn_id must be a valid UUID".to_string(),
            ),
            Self::InvalidUserId => (
                StatusCode::BAD_REQUEST,
                "user_id must be a valid UUID".to_string(),
            ),
            Self::Memory(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Self::Telemetry(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        let body = Json(serde_json::json!({
            "error": { "message": message, "type": "studio_error" }
        }));
        (status, body).into_response()
    }
}

#[derive(Debug, Serialize)]
pub struct UsersResponse {
    pub users: Vec<UserView>,
}

#[derive(Debug, Serialize)]
pub struct UserView {
    pub last_activity_at: u64,
    pub memory_count: u32,
    pub message_count: u32,
    pub score_count: u32,
    pub tool_call_count: u32,
    pub user_id: UserId,
}

impl From<UserSummary> for UserView {
    fn from(s: UserSummary) -> Self {
        Self {
            last_activity_at: s.last_activity_at,
            memory_count: s.memory_count,
            message_count: s.message_count,
            score_count: s.score_count,
            tool_call_count: s.tool_call_count,
            user_id: s.user_id,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MessagesResponse {
    pub messages: Vec<MessageView>,
}

#[derive(Debug, Serialize)]
pub struct MessageView {
    pub content: String,
    pub created_at: u64,
    pub id: String,
    pub role: Role,
    pub token_count: u32,
    /// Tool invocations that fired during this assistant turn, in the order
    /// rig dispatched them. Always empty for user/system messages.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallView>,
}

impl From<StoredMessage> for MessageView {
    fn from(m: StoredMessage) -> Self {
        Self {
            content: m.content,
            created_at: m.created_at,
            id: m.id.0.to_string(),
            role: m.role,
            token_count: m.token_count.0,
            tool_calls: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolCallView {
    pub args: String,
    pub created_at: u64,
    pub error: Option<String>,
    pub id: String,
    pub kind: ToolCallKind,
    pub message_id: String,
    pub ordinal: u32,
    pub result: Option<String>,
    pub tool_name: String,
}

impl From<StoredToolCall> for ToolCallView {
    fn from(t: StoredToolCall) -> Self {
        Self {
            args: t.args,
            created_at: t.created_at,
            error: t.error,
            id: t.id.0.to_string(),
            kind: t.kind,
            message_id: t.message_id.0.to_string(),
            ordinal: t.ordinal,
            result: t.result,
            tool_name: t.tool_name,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MemoriesResponse {
    pub memories: Vec<MemoryView>,
}

/// The embedding vector is dropped on purpose — it's high-cardinality noise
/// for a human reader and would balloon the payload.
#[derive(Debug, Serialize)]
pub struct MemoryView {
    pub content: String,
    pub created_at: u64,
    pub id: MemoryId,
    pub kind: MemoryKind,
}

impl From<Memory> for MemoryView {
    fn from(m: Memory) -> Self {
        Self {
            content: m.content,
            created_at: m.created_at,
            id: m.id,
            kind: m.kind,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ScoresResponse {
    pub averages: Vec<CriterionAverage>,
    pub scores: Vec<ScoreView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScoreView {
    pub created_at: u64,
    pub criterion: String,
    pub id: String,
    pub judge_model: String,
    pub judge_name: String,
    pub message_id: String,
    pub reasoning: String,
    pub score: f32,
}

impl From<Score> for ScoreView {
    fn from(s: Score) -> Self {
        Self {
            created_at: s.created_at,
            criterion: s.criterion,
            id: s.id.0.to_string(),
            judge_model: s.judge_model,
            judge_name: s.judge_name,
            message_id: s.message_id.0.to_string(),
            reasoning: s.reasoning,
            score: s.score,
        }
    }
}

/// Aggregated mean score for one `(judge, criterion)` pair across every
/// assistant turn the user has had. `count` lets the UI distinguish a
/// 9.0 from a single sample versus 9.0 averaged over hundreds.
#[derive(Clone, Debug, Serialize)]
pub struct CriterionAverage {
    pub average: f32,
    pub count: u32,
    pub criterion: String,
    pub judge_name: String,
}

#[derive(Debug, Serialize)]
pub struct EventsResponse {
    pub events: Vec<EventView>,
}

/// One telemetry row surfaced to the studio UI. `parent_id` is used to
/// reconstruct the causal tree on the client — every event roots at the
/// `turn_start` whose `id` matches the turn correlation id.
#[derive(Clone, Debug, Serialize)]
pub struct EventView {
    pub correlation_id: String,
    pub created_at: u64,
    pub duration_ms: Option<u64>,
    pub id: String,
    pub kind: &'static str,
    pub parent_id: Option<String>,
    pub payload: serde_json::Value,
}

impl From<TelemetryEvent> for EventView {
    fn from(e: TelemetryEvent) -> Self {
        Self {
            correlation_id: e.correlation_id.0.to_string(),
            created_at: e.created_at,
            duration_ms: e.duration_ms,
            id: e.id.0.to_string(),
            kind: event_kind_str(e.kind),
            parent_id: e.parent_id.map(|p| p.0.to_string()),
            payload: e.payload,
        }
    }
}

fn event_kind_str(kind: EventKind) -> &'static str {
    match kind {
        EventKind::LlmCall => "llm_call",
        EventKind::ToolCall => "tool_call",
        EventKind::TurnFinish => "turn_finish",
        EventKind::TurnStart => "turn_start",
    }
}
