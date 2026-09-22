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
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use coulisse_core::{EitherFormOrJson, ResponseFormat, redirect_to};
use thiserror::Error;

use crate::config::{SmokeList, SmokeTestConfig};
use crate::dispatcher::{DispatchError, RunDispatcher};
use crate::merge::{AdminSmoke, admin_view};
use crate::store::{SmokeStore, SmokeStoreError};
use crate::types::RunId;
use templates::{SmokePage, SmokeRunPage, SmokeTestDetailPage, SmokeTestEditPage};
use views::{RunDetailView, RunRow, SmokeTestRow};

const RECENT_RUNS_LIMIT: u32 = 25;

/// Everything the smoke admin pages need. Build one in cli and call
/// [`SmokeAdmin::router`] to mount it under `/admin`.
#[derive(Clone)]
pub struct SmokeAdmin {
    pub dispatcher: Arc<dyn RunDispatcher>,
    pub runtime_configs: SmokeList,
    pub store: Arc<SmokeStore>,
    pub yaml_configs: SmokeList,
}

impl SmokeAdmin {
    pub fn router(self) -> Router {
        Router::new()
            .route("/smoke", get(Self::smoke_page).post(Self::create))
            .route("/smoke/new", get(new_form))
            .route("/smoke/runs/{run_id}", get(Self::run_page))
            .route(
                "/smoke/{name}",
                get(Self::test_detail)
                    .put(Self::update)
                    .delete(Self::remove),
            )
            .route("/smoke/{name}/edit", get(Self::edit_form))
            .route("/smoke/{name}/reset", post(Self::reset))
            .route("/smoke/{name}/run", post(Self::run_test))
            .with_state(self)
    }

    async fn create(
        State(state): State<Self>,
        fmt: ResponseFormat,
        EitherFormOrJson(test): EitherFormOrJson<SmokeTestConfig>,
    ) -> Result<Response, AdminError> {
        state.store.put_active_dynamic(&test.name, &test).await?;
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok((StatusCode::CREATED, Json(test)).into_response());
        }
        Ok(redirect_to(&format!("/admin/smoke/{}", test.name)))
    }

    async fn current_admin_view(&self) -> Result<Vec<AdminSmoke>, AdminError> {
        let db = self.store.list_dynamic().await?;
        let yaml = self.yaml_configs.load();
        Ok(admin_view(&yaml, &db))
    }

    async fn edit_form(
        State(state): State<Self>,
        Path(name): Path<String>,
    ) -> Result<Response, AdminError> {
        let admin_rows = state.current_admin_view().await?;
        let row = admin_rows
            .iter()
            .find(|r| r.name == name)
            .ok_or(AdminError::NotFound)?;
        let config = row.config.as_ref().ok_or_else(|| {
            AdminError::BadRequest(
                "cannot edit a tombstoned smoke test — re-enable it first".into(),
            )
        })?;
        let yaml = serde_yaml::to_string(config)?;
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

    async fn rebuild(&self) -> Result<(), AdminError> {
        let yaml = self.yaml_configs.load_full();
        self.store
            .rebuild_smoke(&self.runtime_configs, &yaml)
            .await?;
        Ok(())
    }

    async fn remove(
        State(state): State<Self>,
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
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        Ok(redirect_to("/admin/smoke"))
    }

    async fn reset(
        State(state): State<Self>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let removed = state.store.delete_dynamic(&name).await?;
        if !removed {
            return Err(AdminError::NotFound);
        }
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        Ok(redirect_to(&format!("/admin/smoke/{name}")))
    }

    async fn run_page(
        State(state): State<Self>,
        Path(run_id): Path<String>,
    ) -> Result<Html<String>, AdminError> {
        let run_id = run_id.parse::<RunId>().map_err(AdminError::InvalidRunId)?;
        let run = state
            .store
            .get_run(run_id)
            .await?
            .ok_or(AdminError::NotFound)?;
        let messages = state.store.messages_for_run(run_id).await?;
        let view = RunDetailView::build(&run, messages);
        Ok(Html(SmokeRunPage { run: view }.render()?))
    }

    async fn run_test(
        State(state): State<Self>,
        Path(name): Path<String>,
        headers: HeaderMap,
    ) -> Result<Response, AdminError> {
        let ids = state.dispatcher.dispatch(&name).await?;
        let target = match ids.first() {
            None => format!("/admin/smoke/{name}"),
            Some(id) => format!("/admin/smoke/runs/{id}"),
        };
        if headers.contains_key("hx-request") {
            let mut resp = StatusCode::NO_CONTENT.into_response();
            resp.headers_mut().insert(
                "HX-Redirect",
                HeaderValue::from_str(&target).unwrap_or(HeaderValue::from_static("/admin/smoke")),
            );
            return Ok(resp);
        }
        Ok(Redirect::to(&target).into_response())
    }

    async fn smoke_page(
        State(state): State<Self>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let admin_rows = state.current_admin_view().await?;
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
        State(state): State<Self>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let admin_rows = state.current_admin_view().await?;
        let row = admin_rows
            .iter()
            .find(|r| r.name == name)
            .ok_or(AdminError::NotFound)?;
        if matches!(fmt, ResponseFormat::Json) {
            return match &row.config {
                None => Err(AdminError::NotFound),
                Some(cfg) => Ok(Json(cfg.clone()).into_response()),
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

    async fn update(
        State(state): State<Self>,
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
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(test).into_response());
        }
        Ok(redirect_to(&format!("/admin/smoke/{name}")))
    }
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

#[derive(Debug, Error)]
enum AdminError {
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error("run_id must be a valid UUID: {0}")]
    InvalidRunId(#[source] uuid::Error),
    #[error("not found")]
    NotFound,
    #[error("failed to render page: {0}")]
    Render(#[from] askama::Error),
    #[error("failed to serialize smoke test as YAML: {0}")]
    Serialize(#[from] serde_yaml::Error),
    #[error(transparent)]
    Store(#[from] SmokeStoreError),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::BadRequest(_) | Self::InvalidRunId(_) => StatusCode::BAD_REQUEST,
            Self::Dispatch(DispatchError::NotFound(_)) | Self::NotFound => StatusCode::NOT_FOUND,
            Self::Dispatch(DispatchError::Store(_))
            | Self::Render(_)
            | Self::Serialize(_)
            | Self::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}
