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
use std::time::{SystemTime, UNIX_EPOCH};

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use uuid::Uuid;

use crate::{Sink, TelemetryError, TurnId};
use coulisse_core::UserId;
use templates::{EventsFragment, ToolCallsFragment, ToolDetailPage, ToolsPage};
use views::{event_rows, recent_tool_call_rows, tool_call_rows, tool_detail_row, tool_list_rows};

/// Build the admin router for telemetry. Cli merges this into the
/// combined `/admin` router.
pub fn router(sink: Arc<Sink>) -> Router {
    Router::new()
        .route("/tools", get(tools_page))
        .route("/tools/{name}", get(tool_detail))
        .route("/users/{user_id}/turns/{turn_id}/events", get(turn_events))
        .route(
            "/users/{user_id}/turns/{turn_id}/tool-calls",
            get(turn_tool_calls),
        )
        .with_state(sink)
}

async fn tool_detail(
    State(sink): State<Arc<Sink>>,
    Path(name): Path<String>,
) -> Result<Html<String>, AdminError> {
    let since = now_epoch().saturating_sub(7 * 86_400);
    let stats = sink.tool_call_stats(since).await?;
    let entry = stats
        .into_iter()
        .find(|s| s.tool_name == name)
        .ok_or(AdminError::NotFound)?;
    let calls = sink.tool_calls_for_tool(&name, 20).await?;
    render(ToolDetailPage {
        recent_calls: recent_tool_call_rows(calls),
        tool: tool_detail_row(&entry),
    })
}

async fn tools_page(State(sink): State<Arc<Sink>>) -> Result<Html<String>, AdminError> {
    let since = now_epoch().saturating_sub(7 * 86_400);
    let stats = sink.tool_call_stats(since).await?;
    render(ToolsPage {
        tools: tool_list_rows(stats),
    })
}

async fn turn_events(
    State(sink): State<Arc<Sink>>,
    Path((user_id, turn_id)): Path<(String, String)>,
) -> Result<Html<String>, AdminError> {
    let user_id = parse_user_id(&user_id)?;
    let turn_id = parse_turn_id(&turn_id)?;
    let events = sink.fetch_turn(user_id, turn_id).await?;
    render(EventsFragment {
        rows: event_rows(events),
    })
}

async fn turn_tool_calls(
    State(sink): State<Arc<Sink>>,
    Path((_user_id, turn_id)): Path<(String, String)>,
) -> Result<Html<String>, AdminError> {
    let turn_id = parse_turn_id(&turn_id)?;
    let calls = sink.tool_calls_for_turn(turn_id).await?;
    render(ToolCallsFragment {
        rows: tool_call_rows(calls),
    })
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_user_id(raw: &str) -> Result<UserId, AdminError> {
    Uuid::parse_str(raw)
        .map(UserId::from)
        .map_err(|_| AdminError::InvalidUserId)
}

fn parse_turn_id(raw: &str) -> Result<TurnId, AdminError> {
    Uuid::parse_str(raw)
        .map(TurnId)
        .map_err(|_| AdminError::InvalidTurnId)
}

#[derive(Debug)]
enum AdminError {
    InvalidTurnId,
    InvalidUserId,
    NotFound,
    Render(askama::Error),
    Telemetry(TelemetryError),
}

impl From<TelemetryError> for AdminError {
    fn from(err: TelemetryError) -> Self {
        Self::Telemetry(err)
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
            Self::InvalidTurnId => (
                StatusCode::BAD_REQUEST,
                "turn_id must be a valid UUID".to_string(),
            ),
            Self::InvalidUserId => (
                StatusCode::BAD_REQUEST,
                "user_id must be a valid UUID".to_string(),
            ),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Self::Telemetry(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, message).into_response()
    }
}
