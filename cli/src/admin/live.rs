//! `/admin/live` — a real-time activity board.
//!
//! Renders two cross-feature panels: the `tasks` queue (queued / running /
//! recently finished) and the most recent `tool_calls` from the telemetry
//! crate. The page polls itself via htmx every two seconds; the polling
//! target is the `feed` handler that returns just the inner HTML, so the
//! outer page (sidebar, headings, polling glue) never re-renders.
//!
//! Cross-feature composition lives here rather than in any single feature
//! crate because the data sources span `tasks` and `telemetry`. Matches
//! the project rule: feature crates own their own tables, the cli is the
//! only place that joins them.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use coulisse_core::{TaskState, now_secs};
use tasks::{Task, Tasks};
use telemetry::{Sink as TelemetrySink, ToolCall};

const TASKS_LIMIT: u32 = 20;
const TOOL_CALLS_LIMIT: u32 = 30;

/// The two data sources the live board joins.
#[derive(Clone)]
pub struct LiveBoard {
    pub tasks: Arc<Tasks>,
    pub telemetry: Arc<TelemetrySink>,
}

impl LiveBoard {
    pub fn router(self) -> Router {
        Router::new()
            .route("/live", get(page))
            .route("/live/feed", get(feed))
            .with_state(self)
    }
}

#[derive(Template)]
#[template(path = "live.html")]
struct LivePage;

#[derive(Template)]
#[template(path = "live_feed.html")]
struct LiveFeed {
    calls: Vec<CallRow>,
    tasks: Vec<TaskRow>,
}

struct TaskRow {
    age: String,
    agent: String,
    id_short: String,
    state: &'static str,
}

impl TaskRow {
    fn from_task(t: Task, now: u64) -> Self {
        let reference = match t.state {
            TaskState::Done | TaskState::Errored => t.finished_at.unwrap_or(t.created_at),
            TaskState::Running => t.started_at.unwrap_or(t.created_at),
            TaskState::Queued => t.created_at,
        };
        Self {
            age: Age(now.saturating_sub(reference)).to_string(),
            agent: t.agent,
            id_short: t.id.0.to_string().chars().take(8).collect(),
            state: t.state.as_str(),
        }
    }
}

struct CallRow {
    age: String,
    error: bool,
    kind: &'static str,
    tool_name: String,
}

impl CallRow {
    fn from_call(c: ToolCall, now: u64) -> Self {
        Self {
            age: Age(now.saturating_sub(c.created_at)).to_string(),
            error: c.error.is_some(),
            kind: c.kind.as_str(),
            tool_name: c.tool_name,
        }
    }
}

async fn page() -> Result<Html<String>, LiveError> {
    Ok(Html(LivePage.render()?))
}

async fn feed(State(board): State<LiveBoard>) -> Result<Html<String>, LiveError> {
    let tasks = board.tasks.recent(TASKS_LIMIT).await?;
    let calls = board.telemetry.recent_tool_calls(TOOL_CALLS_LIMIT).await?;

    let now = now_secs();
    let html = LiveFeed {
        calls: calls
            .into_iter()
            .map(|c| CallRow::from_call(c, now))
            .collect(),
        tasks: tasks
            .into_iter()
            .map(|t| TaskRow::from_task(t, now))
            .collect(),
    }
    .render()?;
    Ok(Html(html))
}

/// Elapsed seconds, rendered coarsely ("3m ago") for the board.
struct Age(u64);

impl std::fmt::Display for Age {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let delta = self.0;
        if delta < 60 {
            write!(f, "{delta}s ago")
        } else if delta < 3_600 {
            write!(f, "{}m ago", delta / 60)
        } else if delta < 86_400 {
            write!(f, "{}h ago", delta / 3_600)
        } else {
            write!(f, "{}d ago", delta / 86_400)
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum LiveError {
    #[error("live page render failed: {0}")]
    Render(#[from] askama::Error),
    #[error("task queue read failed: {0}")]
    Tasks(#[from] tasks::TaskError),
    #[error("telemetry read failed: {0}")]
    Telemetry(#[from] telemetry::TelemetryError),
}

impl IntoResponse for LiveError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self, "live board request failed");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}
