//! Admin/studio HTTP surface for the memory crate. Exposes two pages —
//! the user list and a per-user conversation view — both as HTML fragments
//! suitable for htmx swaps. Cli wraps non-htmx responses in its base layout.
//!
//! Cross-feature panels (judge scores, tool calls, telemetry events) on
//! the conversation page are filled in via htmx hits to other feature
//! crates' admin routers; this module never reaches outside `Store`.

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use uuid::Uuid;

use crate::{MemoryError, Store, UserId};
use templates::{AgentRecentConversationsFragment, ConversationPage, ConversationsPage};
use views::{AgentConversationRow, MemoryRow, message_rows};

/// Build the admin router for memory. Cli merges this into the combined
/// `/admin` router and applies the admin auth scope.
pub fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route(
            "/agents/{name}/recent-conversations",
            get(agent_recent_conversations),
        )
        .route("/conversations", get(conversations))
        .route("/conversations/{user_id}", get(conversation))
        .with_state(store)
}

async fn agent_recent_conversations(
    State(store): State<Arc<Store>>,
    Path(name): Path<String>,
) -> Result<Html<String>, AdminError> {
    let _ = name;
    let conversations: Vec<AgentConversationRow> = store
        .conversation_summaries()
        .await?
        .into_iter()
        .take(10)
        .map(Into::into)
        .collect();
    render(AgentRecentConversationsFragment { conversations })
}

async fn conversations(State(store): State<Arc<Store>>) -> Result<Html<String>, AdminError> {
    let conversations = store
        .conversation_summaries()
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    render(ConversationsPage { conversations })
}

async fn conversation(
    State(store): State<Arc<Store>>,
    Path(user_id): Path<String>,
) -> Result<Html<String>, AdminError> {
    let user_id = parse_user_id(&user_id)?;
    let um = store.for_user(user_id);
    let messages = um.messages().await?;
    let memories: Vec<MemoryRow> = um.memories().await?.into_iter().map(Into::into).collect();
    render(ConversationPage {
        memories,
        messages: message_rows(messages),
        user_id: user_id.0.to_string(),
    })
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

fn parse_user_id(raw: &str) -> Result<UserId, AdminError> {
    Uuid::parse_str(raw)
        .map(UserId::from)
        .map_err(|_| AdminError::InvalidUserId)
}

#[derive(Debug)]
enum AdminError {
    InvalidUserId,
    Memory(MemoryError),
    Render(askama::Error),
}

impl From<MemoryError> for AdminError {
    fn from(err: MemoryError) -> Self {
        Self::Memory(err)
    }
}

impl From<askama::Error> for AdminError {
    fn from(err: askama::Error) -> Self {
        Self::Render(err)
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
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, message).into_response()
    }
}
