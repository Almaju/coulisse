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
use memory::{Embedder, Memory, MemoryId, MemoryKind, Role, StoredMessage, UserId, UserSummary};
use prompter::Prompter;
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;

pub fn router<E: Embedder + 'static, P: Prompter + 'static>() -> Router<Arc<AppState<E, P>>> {
    Router::new()
        .route("/users", get(list_users::<E, P>))
        .route("/users/{user_id}/memories", get(user_memories::<E, P>))
        .route("/users/{user_id}/messages", get(user_messages::<E, P>))
}

async fn list_users<E: Embedder, P: Prompter>(
    State(state): State<Arc<AppState<E, P>>>,
) -> Json<UsersResponse> {
    let users = state
        .memory
        .list_user_summaries()
        .await
        .into_iter()
        .map(UserView::from)
        .collect();
    Json(UsersResponse { users })
}

async fn user_messages<E: Embedder, P: Prompter>(
    State(state): State<Arc<AppState<E, P>>>,
    Path(user_id): Path<String>,
) -> Result<Json<MessagesResponse>, AdminError> {
    let user_id = parse_user_id(&user_id)?;
    let um = state.memory.for_user(user_id).await;
    let messages = um
        .messages()
        .await
        .into_iter()
        .map(MessageView::from)
        .collect();
    Ok(Json(MessagesResponse { messages }))
}

async fn user_memories<E: Embedder, P: Prompter>(
    State(state): State<Arc<AppState<E, P>>>,
    Path(user_id): Path<String>,
) -> Result<Json<MemoriesResponse>, AdminError> {
    let user_id = parse_user_id(&user_id)?;
    let um = state.memory.for_user(user_id).await;
    let memories = um
        .memories()
        .await
        .into_iter()
        .map(MemoryView::from)
        .collect();
    Ok(Json(MemoriesResponse { memories }))
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
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidUserId => (StatusCode::BAD_REQUEST, "user_id must be a valid UUID"),
        };
        let body = Json(serde_json::json!({
            "error": { "message": message, "type": "invalid_request" }
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
    pub user_id: UserId,
}

impl From<UserSummary> for UserView {
    fn from(s: UserSummary) -> Self {
        Self {
            last_activity_at: s.last_activity_at,
            memory_count: s.memory_count,
            message_count: s.message_count,
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
