//! Read-only JSON endpoints for the admin UI.
//!
//! These live under `/admin/api/*` and are intentionally minimal: list users,
//! read one user's messages, read one user's long-term memories. Writes and
//! auth are deliberately out of scope — see `docs/src/features/admin-ui.md`.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use memory::{
    Memory, MemoryError, MemoryId, MemoryKind, Role, Score, StoredMessage, UserId, UserSummary,
};
use prompter::Prompter;
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;

pub fn router<P: Prompter + 'static>() -> Router<Arc<AppState<P>>> {
    Router::new()
        .route("/users", get(list_users::<P>))
        .route("/users/{user_id}/memories", get(user_memories::<P>))
        .route("/users/{user_id}/messages", get(user_messages::<P>))
        .route("/users/{user_id}/scores", get(user_scores::<P>))
}

async fn list_users<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
) -> Result<Json<UsersResponse>, AdminError> {
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
) -> Result<Json<MessagesResponse>, AdminError> {
    let user_id = parse_user_id(&user_id)?;
    let um = state.memory.for_user(user_id);
    let messages = um
        .messages()
        .await?
        .into_iter()
        .map(MessageView::from)
        .collect();
    Ok(Json(MessagesResponse { messages }))
}

async fn user_memories<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
    Path(user_id): Path<String>,
) -> Result<Json<MemoriesResponse>, AdminError> {
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

async fn user_scores<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
    Path(user_id): Path<String>,
) -> Result<Json<ScoresResponse>, AdminError> {
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

/// Admin endpoints expect a real UUID in the path — unlike chat requests,
/// there's no sensible way to derive one from arbitrary strings here, since
/// the caller is trying to look up a specific pre-existing record.
fn parse_user_id(raw: &str) -> Result<UserId, AdminError> {
    Uuid::parse_str(raw)
        .map(UserId::from)
        .map_err(|_| AdminError::InvalidUserId)
}

#[derive(Debug)]
enum AdminError {
    InvalidUserId,
    Memory(MemoryError),
}

impl From<MemoryError> for AdminError {
    fn from(err: MemoryError) -> Self {
        Self::Memory(err)
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidUserId => (
                StatusCode::BAD_REQUEST,
                "user_id must be a valid UUID".to_string(),
            ),
            Self::Memory(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        let body = Json(serde_json::json!({
            "error": { "message": message, "type": "admin_error" }
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
    pub user_id: UserId,
}

impl From<UserSummary> for UserView {
    fn from(s: UserSummary) -> Self {
        Self {
            last_activity_at: s.last_activity_at,
            memory_count: s.memory_count,
            message_count: s.message_count,
            score_count: s.score_count,
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
}

impl From<StoredMessage> for MessageView {
    fn from(m: StoredMessage) -> Self {
        Self {
            content: m.content,
            created_at: m.created_at,
            id: m.id.0.to_string(),
            role: m.role,
            token_count: m.token_count.0,
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
