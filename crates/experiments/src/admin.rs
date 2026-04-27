//! Admin/studio HTTP surface for the experiments crate. Per-experiment
//! bandit metrics load via htmx from the judges admin router
//! (`/admin/scores/means`), so this module never depends on `judges`.
//!
//! Edits write to `dynamic_experiments` in the database; the YAML file
//! is never modified. Resolution at runtime is "DB wins, YAML fallback."
//! The in-memory `ExperimentRouter` that consumes these configs still
//! requires a process restart to swap (documented limitation).

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use coulisse_core::{EitherFormOrJson, ResponseFormat, redirect_to};

use crate::merge::{AdminExperiment, admin_view};
use crate::store::{Experiments, ExperimentsError};
use crate::{ExperimentConfig, ExperimentList};
use templates::{ExperimentEditPage, ExperimentsPage};
use views::ExperimentRow;

#[derive(Clone)]
struct AdminState {
    runtime_experiments: ExperimentList,
    store: Arc<Experiments>,
    yaml_experiments: ExperimentList,
}

pub fn router(
    runtime_experiments: ExperimentList,
    store: Arc<Experiments>,
    yaml_experiments: ExperimentList,
) -> Router {
    let state = AdminState {
        runtime_experiments,
        store,
        yaml_experiments,
    };
    Router::new()
        .route("/experiments", get(list).post(create))
        .route("/experiments/new", get(new_form))
        .route(
            "/experiments/{name}",
            get(detail).put(update).delete(remove),
        )
        .route("/experiments/{name}/edit", get(edit_form))
        .route("/experiments/{name}/reset", post(reset))
        .with_state(state)
}

async fn list(
    State(state): State<AdminState>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let rows = current_admin_view(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        let configs: Vec<&ExperimentConfig> =
            rows.iter().filter_map(|r| r.config.as_ref()).collect();
        return Ok(Json(configs).into_response());
    }
    let view: Vec<ExperimentRow> = rows.iter().map(ExperimentRow::from_admin).collect();
    Ok(Html(ExperimentsPage { experiments: view }.render()?).into_response())
}

async fn detail(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let rows = current_admin_view(&state).await?;
    let row = rows
        .iter()
        .find(|r| r.name == name)
        .ok_or(AdminError::NotFound)?;
    if matches!(fmt, ResponseFormat::Json) {
        return match &row.config {
            Some(cfg) => Ok(Json(cfg.clone()).into_response()),
            None => Err(AdminError::NotFound),
        };
    }
    // No bespoke detail page; the list page already renders everything
    // compactly. For HTML we redirect into the list anchored at the
    // experiment so users can read its block without losing context.
    let mut resp = (
        StatusCode::SEE_OTHER,
        [("location", format!("/admin/experiments#{name}"))],
    )
        .into_response();
    resp.headers_mut().insert(
        "hx-redirect",
        axum::http::HeaderValue::from_str(&format!("/admin/experiments#{name}"))
            .expect("valid header value"),
    );
    Ok(resp)
}

async fn create(
    State(state): State<AdminState>,
    fmt: ResponseFormat,
    EitherFormOrJson(experiment): EitherFormOrJson<ExperimentConfig>,
) -> Result<Response, AdminError> {
    state
        .store
        .put_active_dynamic(&experiment.name, &experiment)
        .await?;
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok((StatusCode::CREATED, Json(experiment)).into_response());
    }
    Ok(redirect_to("/admin/experiments"))
}

async fn update(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
    EitherFormOrJson(experiment): EitherFormOrJson<ExperimentConfig>,
) -> Result<Response, AdminError> {
    if experiment.name != name {
        return Err(AdminError::BadRequest(format!(
            "URL experiment name '{name}' does not match body name '{}'",
            experiment.name
        )));
    }
    state.store.put_active_dynamic(&name, &experiment).await?;
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(experiment).into_response());
    }
    Ok(redirect_to("/admin/experiments"))
}

async fn remove(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let yaml_backed = state.yaml_experiments.load().iter().any(|c| c.name == name);
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
    Ok(redirect_to("/admin/experiments"))
}

async fn reset(
    State(state): State<AdminState>,
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
    Ok(redirect_to("/admin/experiments"))
}

async fn edit_form(
    State(state): State<AdminState>,
    Path(name): Path<String>,
) -> Result<Response, AdminError> {
    let rows = current_admin_view(&state).await?;
    let row = rows
        .iter()
        .find(|r| r.name == name)
        .ok_or(AdminError::NotFound)?;
    let config = row.config.as_ref().ok_or_else(|| {
        AdminError::BadRequest("cannot edit a tombstoned experiment — re-enable it first".into())
    })?;
    let yaml =
        serde_yaml::to_string(config).map_err(|err| AdminError::Internal(err.to_string()))?;
    Ok(Html(
        ExperimentEditPage {
            action: format!("/admin/experiments/{name}"),
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
    let yaml = "name: \nstrategy: split\nvariants:\n  - agent: \n    weight: 1.0\n".to_string();
    Ok(Html(
        ExperimentEditPage {
            action: "/admin/experiments".to_string(),
            is_new: true,
            method: "post",
            name: String::new(),
            yaml,
        }
        .render()?,
    )
    .into_response())
}

async fn current_admin_view(state: &AdminState) -> Result<Vec<AdminExperiment>, AdminError> {
    let db = state.store.list_dynamic().await?;
    let yaml = state.yaml_experiments.load();
    Ok(admin_view(&yaml, &db))
}

async fn rebuild(state: &AdminState) -> Result<(), AdminError> {
    let yaml = state.yaml_experiments.load_full();
    state
        .store
        .rebuild(&state.runtime_experiments, &yaml)
        .await?;
    Ok(())
}

#[derive(Debug)]
enum AdminError {
    BadRequest(String),
    Experiments(ExperimentsError),
    Internal(String),
    NotFound,
    Render(askama::Error),
}

impl From<askama::Error> for AdminError {
    fn from(err: askama::Error) -> Self {
        Self::Render(err)
    }
}

impl From<ExperimentsError> for AdminError {
    fn from(err: ExperimentsError) -> Self {
        Self::Experiments(err)
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Experiments(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            Self::NotFound => (StatusCode::NOT_FOUND, "experiment not found".to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, msg).into_response()
    }
}
