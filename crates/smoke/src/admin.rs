//! Admin/studio HTTP surface for the smoke crate. Pages: list of
//! configured tests, per-test detail, run viewer, plus CRUD endpoints.
//!
//! Test writes go to `dynamic_smoke_tests` in the database; the YAML file
//! is never modified. Resolution at runtime is "DB wins, YAML fallback."
//! "Run now" delegates to a [`RunDispatcher`] (implemented in cli, since
//! it owns agents + judges).

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use coulisse_core::{EitherFormOrJson, ResponseFormat, redirect_to};
use uuid::Uuid;

use crate::config::{SmokeList, SmokeTestConfig};
use crate::dispatcher::{DispatchError, RunDispatcher};
use crate::merge::{AdminSmoke, admin_view};
use crate::store::{SmokeStore, SmokeStoreError};
use crate::types::RunId;
use templates::{SmokePage, SmokeRunPage, SmokeTestDetailPage, SmokeTestEditPage};
use views::{RunDetailView, RunRow, SmokeTestRow};

const RECENT_RUNS_LIMIT: u32 = 25;

#[derive(Clone)]
struct SmokeAdminState {
    dispatcher: Arc<dyn RunDispatcher>,
    runtime_configs: SmokeList,
    store: Arc<SmokeStore>,
    yaml_configs: SmokeList,
}

pub fn router(
    runtime_configs: SmokeList,
    store: Arc<SmokeStore>,
    dispatcher: Arc<dyn RunDispatcher>,
    yaml_configs: SmokeList,
) -> Router {
    Router::new()
        .route("/smoke", get(smoke_page).post(create))
        .route("/smoke/new", get(new_form))
        .route("/smoke/runs/{run_id}", get(run_page))
        .route("/smoke/{name}", get(test_detail).put(update).delete(remove))
        .route("/smoke/{name}/edit", get(edit_form))
        .route("/smoke/{name}/reset", post(reset))
        .route("/smoke/{name}/run", post(run_test))
        .with_state(SmokeAdminState {
            dispatcher,
            runtime_configs,
            store,
            yaml_configs,
        })
}

async fn smoke_page(
    State(state): State<SmokeAdminState>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let admin_rows = current_admin_view(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        let configs: Vec<&SmokeTestConfig> = admin_rows
            .iter()
            .filter_map(|r| r.config.as_ref())
            .collect();
        return Ok(Json(configs).into_response());
    }
    let runs = state.store.list_runs(RECENT_RUNS_LIMIT).await?;
    let tests: Vec<SmokeTestRow> = admin_rows
        .iter()
        .map(|row| {
            let last = runs.iter().find(|r| r.test_name == row.name);
            SmokeTestRow::from_admin(row, last)
        })
        .collect();
    Ok(Html(SmokePage { tests }.render()?).into_response())
}

async fn test_detail(
    State(state): State<SmokeAdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let admin_rows = current_admin_view(&state).await?;
    let row = admin_rows
        .iter()
        .find(|r| r.name == name)
        .ok_or(AdminError::NotFound)?;
    if matches!(fmt, ResponseFormat::Json) {
        return match &row.config {
            Some(cfg) => Ok(Json(cfg.clone()).into_response()),
            None => Err(AdminError::NotFound),
        };
    }
    let runs = state
        .store
        .list_runs_for_test(&name, RECENT_RUNS_LIMIT)
        .await?;
    let recent_runs: Vec<RunRow> = runs.iter().map(RunRow::build).collect();
    let test = SmokeTestRow::from_admin(row, runs.first());
    Ok(Html(SmokeTestDetailPage { recent_runs, test }.render()?).into_response())
}

async fn create(
    State(state): State<SmokeAdminState>,
    fmt: ResponseFormat,
    EitherFormOrJson(test): EitherFormOrJson<SmokeTestConfig>,
) -> Result<Response, AdminError> {
    state.store.put_active_dynamic(&test.name, &test).await?;
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok((StatusCode::CREATED, Json(test)).into_response());
    }
    Ok(redirect_to(&format!("/admin/smoke/{}", test.name)))
}

async fn update(
    State(state): State<SmokeAdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
    EitherFormOrJson(test): EitherFormOrJson<SmokeTestConfig>,
) -> Result<Response, AdminError> {
    if test.name != name {
        return Err(AdminError::BadRequest(format!(
            "URL test name '{name}' does not match body name '{}'",
            test.name
        )));
    }
    state.store.put_active_dynamic(&name, &test).await?;
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(test).into_response());
    }
    Ok(redirect_to(&format!("/admin/smoke/{name}")))
}

async fn remove(
    State(state): State<SmokeAdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let yaml_backed = state.yaml_configs.load().iter().any(|c| c.name == name);
    let exists_in_db = state
        .store
        .list_dynamic()
        .await?
        .iter()
        .any(|r| r.name == name);
    if !yaml_backed && !exists_in_db {
        return Err(AdminError::NotFound);
    }
    if yaml_backed {
        state.store.put_tombstone_dynamic(&name).await?;
    } else {
        state.store.delete_dynamic(&name).await?;
    }
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    Ok(redirect_to("/admin/smoke"))
}

async fn reset(
    State(state): State<SmokeAdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let removed = state.store.delete_dynamic(&name).await?;
    if !removed {
        return Err(AdminError::NotFound);
    }
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    Ok(redirect_to(&format!("/admin/smoke/{name}")))
}

async fn edit_form(
    State(state): State<SmokeAdminState>,
    Path(name): Path<String>,
) -> Result<Response, AdminError> {
    let admin_rows = current_admin_view(&state).await?;
    let row = admin_rows
        .iter()
        .find(|r| r.name == name)
        .ok_or(AdminError::NotFound)?;
    let config = row.config.as_ref().ok_or_else(|| {
        AdminError::BadRequest("cannot edit a tombstoned smoke test — re-enable it first".into())
    })?;
    let yaml =
        serde_yaml::to_string(config).map_err(|err| AdminError::Internal(err.to_string()))?;
    Ok(Html(
        SmokeTestEditPage {
            action: format!("/admin/smoke/{name}"),
            is_new: false,
            method: "put",
            name,
            yaml,
        }
        .render()?,
    )
    .into_response())
}

async fn new_form() -> Result<Response, AdminError> {
    let yaml = "name: \ntarget: \npersona:\n  provider: openai\n  model: \n  preamble: \nrepetitions: 1\nmax_turns: 10\n".to_string();
    Ok(Html(
        SmokeTestEditPage {
            action: "/admin/smoke".to_string(),
            is_new: true,
            method: "post",
            name: String::new(),
            yaml,
        }
        .render()?,
    )
    .into_response())
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
    Ok(Html(SmokeRunPage { run: view }.render()?))
}

async fn current_admin_view(state: &SmokeAdminState) -> Result<Vec<AdminSmoke>, AdminError> {
    let db = state.store.list_dynamic().await?;
    let yaml = state.yaml_configs.load();
    Ok(admin_view(&yaml, &db))
}

async fn rebuild(state: &SmokeAdminState) -> Result<(), AdminError> {
    let yaml = state.yaml_configs.load_full();
    state
        .store
        .rebuild_smoke(&state.runtime_configs, &yaml)
        .await?;
    Ok(())
}

fn parse_run_id(raw: &str) -> Result<RunId, AdminError> {
    Uuid::parse_str(raw)
        .map(RunId)
        .map_err(|_| AdminError::InvalidRunId)
}

#[derive(Debug)]
enum AdminError {
    BadRequest(String),
    Dispatch(DispatchError),
    Internal(String),
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
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Dispatch(DispatchError::NotFound(name)) => (
                StatusCode::NOT_FOUND,
                format!("smoke test '{name}' not found"),
            ),
            Self::Dispatch(DispatchError::Other(m)) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
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
