//! Admin/studio HTTP surface for the smoke crate. Three pages: a list
//! of configured tests, a per-test detail view with recent runs, and
//! the run viewer (transcript + status). The "Run now" button POSTs
//! to `/admin/smoke/{name}/run`, which delegates to a [`RunDispatcher`]
//! (implemented in cli, since it owns agents + judges).

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use uuid::Uuid;

use crate::config::SmokeList;
use crate::dispatcher::{DispatchError, RunDispatcher};
use crate::store::{SmokeStore, SmokeStoreError};
use crate::types::RunId;
use templates::{SmokePage, SmokeRunPage, SmokeTestDetailPage};
use views::{RunDetailView, RunRow, SmokeTestRow};

const RECENT_RUNS_LIMIT: u32 = 25;

#[derive(Clone)]
struct SmokeAdminState {
    configs: SmokeList,
    dispatcher: Arc<dyn RunDispatcher>,
    store: Arc<SmokeStore>,
}

/// Build the smoke admin router. Cli merges this into the combined
/// `/admin` router.
pub fn router(
    configs: SmokeList,
    store: Arc<SmokeStore>,
    dispatcher: Arc<dyn RunDispatcher>,
) -> Router {
    Router::new()
        .route("/smoke", get(smoke_page))
        .route("/smoke/runs/{run_id}", get(run_page))
        .route("/smoke/{name}", get(test_detail))
        .route("/smoke/{name}/run", post(run_test))
        .with_state(SmokeAdminState {
            configs,
            dispatcher,
            store,
        })
}

async fn smoke_page(State(state): State<SmokeAdminState>) -> Result<Html<String>, AdminError> {
    let snapshot = state.configs.load();
    let runs = state.store.list_runs(RECENT_RUNS_LIMIT).await?;
    let mut tests: Vec<SmokeTestRow> = snapshot
        .iter()
        .map(|cfg| {
            let last = runs.iter().find(|r| r.test_name == cfg.name);
            SmokeTestRow::build(cfg, last)
        })
        .collect();
    tests.sort_by(|a, b| a.name.cmp(&b.name));
    render(SmokePage { tests })
}

async fn test_detail(
    State(state): State<SmokeAdminState>,
    Path(name): Path<String>,
) -> Result<Html<String>, AdminError> {
    let snapshot = state.configs.load();
    let config = snapshot
        .iter()
        .find(|c| c.name == name)
        .ok_or(AdminError::NotFound)?;
    let runs = state
        .store
        .list_runs_for_test(&name, RECENT_RUNS_LIMIT)
        .await?;
    let recent_runs: Vec<RunRow> = runs.iter().map(RunRow::build).collect();
    let test = SmokeTestRow::build(config, runs.first());
    render(SmokeTestDetailPage { recent_runs, test })
}

async fn run_test(
    State(state): State<SmokeAdminState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AdminError> {
    let ids = state.dispatcher.dispatch(&name).await?;
    let target = match ids.first() {
        Some(id) => format!("/admin/smoke/runs/{}", id.0),
        None => format!("/admin/smoke/{name}"),
    };
    if headers.contains_key("hx-request") {
        let mut resp = StatusCode::NO_CONTENT.into_response();
        resp.headers_mut().insert(
            "HX-Redirect",
            axum::http::HeaderValue::from_str(&target)
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("/admin/smoke")),
        );
        return Ok(resp);
    }
    Ok(Redirect::to(&target).into_response())
}

async fn run_page(
    State(state): State<SmokeAdminState>,
    Path(run_id): Path<String>,
) -> Result<Html<String>, AdminError> {
    let run_id = parse_run_id(&run_id)?;
    let run = state
        .store
        .get_run(run_id)
        .await?
        .ok_or(AdminError::NotFound)?;
    let messages = state.store.messages_for_run(run_id).await?;
    let view = RunDetailView::build(&run, messages);
    render(SmokeRunPage { run: view })
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

fn parse_run_id(raw: &str) -> Result<RunId, AdminError> {
    Uuid::parse_str(raw)
        .map(RunId)
        .map_err(|_| AdminError::InvalidRunId)
}

#[derive(Debug)]
enum AdminError {
    Dispatch(DispatchError),
    InvalidRunId,
    NotFound,
    Render(askama::Error),
    Store(SmokeStoreError),
}

impl From<SmokeStoreError> for AdminError {
    fn from(err: SmokeStoreError) -> Self {
        Self::Store(err)
    }
}

impl From<askama::Error> for AdminError {
    fn from(err: askama::Error) -> Self {
        Self::Render(err)
    }
}

impl From<DispatchError> for AdminError {
    fn from(err: DispatchError) -> Self {
        Self::Dispatch(err)
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Dispatch(DispatchError::NotFound(name)) => (
                StatusCode::NOT_FOUND,
                format!("smoke test '{name}' not found"),
            ),
            Self::Dispatch(DispatchError::Other(m)) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            Self::InvalidRunId => (
                StatusCode::BAD_REQUEST,
                "run_id must be a valid UUID".to_string(),
            ),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Self::Store(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, message).into_response()
    }
}
