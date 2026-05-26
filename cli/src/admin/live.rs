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
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use coulisse_core::now_secs;
use tasks::{Task, TaskState, Tasks};
use telemetry::{Sink as TelemetrySink, ToolCall};

const TASKS_LIMIT: u32 = 20;
const TOOL_CALLS_LIMIT: u32 = 30;

#[derive(Clone)]
pub struct State {
    pub tasks: Arc<Tasks>,
    pub telemetry: Arc<TelemetrySink>,
}

pub fn router(state: State) -> Router {
    Router::new()
        .route("/live", get(page))
        .route("/live/feed", get(feed))
        .with_state(state)
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

struct CallRow {
    age: String,
    error: bool,
    kind: &'static str,
    tool_name: String,
}

async fn page() -> Result<Html<String>, StatusCode> {
    LivePage
        .render()
        .map(Html)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn feed(AxumState(state): AxumState<State>) -> Result<Html<String>, StatusCode> {
    let tasks = state
        .tasks
        .recent(TASKS_LIMIT)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let calls = state
        .telemetry
        .recent_tool_calls(TOOL_CALLS_LIMIT)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let now = now_secs();
    LiveFeed {
        calls: calls.into_iter().map(|c| call_row(c, now)).collect(),
        tasks: tasks.into_iter().map(|t| task_row(t, now)).collect(),
    }
    .render()
    .map(Html)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn task_row(t: Task, now: u64) -> TaskRow {
    let reference = match t.state {
        TaskState::Done | TaskState::Errored => t.finished_at.unwrap_or(t.created_at),
        TaskState::Running => t.started_at.unwrap_or(t.created_at),
        TaskState::Queued => t.created_at,
    };
    TaskRow {
        age: age_human(reference, now),
        agent: t.agent,
        id_short: t.id.0.to_string().chars().take(8).collect(),
        state: t.state.as_str(),
    }
}

fn call_row(c: ToolCall, now: u64) -> CallRow {
    CallRow {
        age: age_human(c.created_at, now),
        error: c.error.is_some(),
        kind: kind_label(c.kind),
        tool_name: c.tool_name,
    }
}

fn kind_label(k: coulisse_core::ToolCallKind) -> &'static str {
    k.as_str()
}

fn age_human(then: u64, now: u64) -> String {
    let delta = now.saturating_sub(then);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3_600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
}
