//! Admin/studio HTTP surface for the telemetry crate. Two htmx fragments,
//! both keyed on the assistant message id (which the chat handler reuses
//! as the turn correlation id):
//!
//! - per-message tool-call panel rendered above each assistant message
//! - per-message event tree rendered inside the "Telemetry" expander
//!
//! Memory's conversation page hits both endpoints via `hx-get`. This
//! module never reaches outside `Sink`.

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use coulisse_core::{UserId, now_secs};

use crate::{Sink, TelemetryError, TurnId};
use templates::{EventsFragment, ToolCallsFragment, ToolDetailPage, ToolsPage};
use views::{EventTree, RecentToolCallRow, ToolCallRow, ToolDetailRow, ToolListRow};

const STATS_WINDOW_SECS: u64 = 7 * 86_400;

/// Handler state for the telemetry admin pages. Cli merges
/// [`TelemetryAdmin::router`] into the combined `/admin` router.
#[derive(Clone)]
pub struct TelemetryAdmin {
    sink: Arc<Sink>,
}

impl TelemetryAdmin {
    #[must_use]
    pub fn new(sink: Arc<Sink>) -> Self {
        Self { sink }
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/tools", get(Self::tools_page))
            .route("/tools/{name}", get(Self::tool_detail))
            .route(
                "/users/{user_id}/turns/{turn_id}/events",
                get(Self::turn_events),
            )
            .route(
                "/users/{user_id}/turns/{turn_id}/tool-calls",
                get(Self::turn_tool_calls),
            )
            .with_state(self)
    }

    async fn tool_detail(
        State(admin): State<Self>,
        Path(name): Path<String>,
    ) -> Result<Html<String>, AdminError> {
        let now = now_secs();
        let stats = admin
            .sink
            .tool_call_stats(now.saturating_sub(STATS_WINDOW_SECS))
            .await?;
        let entry = stats
            .into_iter()
            .find(|s| s.tool_name == name)
            .ok_or(AdminError::NotFound)?;
        let calls = admin.sink.tool_calls_for_tool(&name, 20).await?;
        render(ToolDetailPage {
            recent_calls: calls
                .into_iter()
                .map(|call| RecentToolCallRow::new(call, now))
                .collect(),
            tool: ToolDetailRow::from(&entry),
        })
    }

    async fn tools_page(State(admin): State<Self>) -> Result<Html<String>, AdminError> {
        let since = now_secs().saturating_sub(STATS_WINDOW_SECS);
        let stats = admin.sink.tool_call_stats(since).await?;
        render(ToolsPage {
            tools: stats.into_iter().map(ToolListRow::from).collect(),
        })
    }

    async fn turn_events(
        State(admin): State<Self>,
        Path((user_id, turn_id)): Path<(String, String)>,
    ) -> Result<Html<String>, AdminError> {
        let user_id = user_id
            .parse::<UserId>()
            .map_err(AdminError::InvalidUserId)?;
        let turn_id = turn_id
            .parse::<TurnId>()
            .map_err(AdminError::InvalidTurnId)?;
        let events = admin.sink.fetch_turn(user_id, turn_id).await?;
        render(EventsFragment {
            rows: EventTree::from(events).into_rows(),
        })
    }

    async fn turn_tool_calls(
        State(admin): State<Self>,
        Path((_user_id, turn_id)): Path<(String, String)>,
    ) -> Result<Html<String>, AdminError> {
        let turn_id = turn_id
            .parse::<TurnId>()
            .map_err(AdminError::InvalidTurnId)?;
        let calls = admin.sink.tool_calls_for_turn(turn_id).await?;
        render(ToolCallsFragment {
            rows: calls.into_iter().map(ToolCallRow::from).collect(),
        })
    }
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

#[derive(Debug, thiserror::Error)]
enum AdminError {
    #[error("turn_id must be a valid UUID")]
    InvalidTurnId(#[source] uuid::Error),
    #[error("user_id must be a valid UUID")]
    InvalidUserId(#[source] uuid::Error),
    #[error("not found")]
    NotFound,
    #[error("template render failed: {0}")]
    Render(#[from] askama::Error),
    #[error("{0}")]
    Telemetry(#[from] TelemetryError),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::InvalidTurnId(_) | Self::InvalidUserId(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Render(_) | Self::Telemetry(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "telemetry admin request failed");
        }
        (status, self.to_string()).into_response()
    }
}
