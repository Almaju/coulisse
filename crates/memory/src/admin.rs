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

use crate::{MemoryError, Store, UserId};
use templates::{AgentRecentConversationsFragment, ConversationPage, ConversationsPage};
use views::{AgentConversationRow, MemoryRow, message_rows};

impl Store {
    /// Build the admin router for memory. Cli merges this into the combined
    /// `/admin` router and applies the admin auth scope.
    pub fn admin_router(self: Arc<Self>) -> Router {
        Router::new()
            .route(
                "/agents/{name}/recent-conversations",
                get(Self::agent_recent_conversations),
            )
            .route("/conversations", get(Self::conversations))
            .route("/conversations/{user_id}", get(Self::conversation))
            .with_state(self)
    }

    async fn agent_recent_conversations(
        State(store): State<Arc<Self>>,
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

    async fn conversation(
        State(store): State<Arc<Self>>,
        Path(user_id): Path<String>,
    ) -> Result<Html<String>, AdminError> {
        let user_id = user_id
            .parse::<UserId>()
            .map_err(AdminError::InvalidUserId)?;
        let um = store.for_user(user_id);
        let messages = um.messages().await?;
        let memories: Vec<MemoryRow> = um.memories().await?.into_iter().map(Into::into).collect();
        render(ConversationPage {
            memories,
            messages: message_rows(messages),
            user_id,
        })
    }

    async fn conversations(State(store): State<Arc<Self>>) -> Result<Html<String>, AdminError> {
        let conversations = store
            .conversation_summaries()
            .await?
            .into_iter()
            .map(Into::into)
            .collect();
        render(ConversationsPage { conversations })
    }
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

#[derive(Debug, thiserror::Error)]
enum AdminError {
    #[error("user_id must be a valid UUID")]
    InvalidUserId(#[source] uuid::Error),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Render(#[from] askama::Error),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::InvalidUserId(_) => StatusCode::BAD_REQUEST,
            Self::Memory(_) | Self::Render(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "memory admin request failed");
        }
        (status, self.to_string()).into_response()
    }
}
