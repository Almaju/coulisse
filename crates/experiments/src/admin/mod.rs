//! Admin/studio HTTP surface for the experiments crate. Per-experiment
//! bandit metrics load via htmx from the judges admin router
//! (`/admin/scores/means`), so this module never depends on `judges`.
//!
//! Edits write back to `coulisse.yaml` through the cli's
//! `ConfigPersister`. The admin display reflects the file in real
//! time; the in-memory `ExperimentRouter` that consumes these configs
//! still requires a process restart to swap (documented limitation).

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use coulisse_core::{ConfigPersistError, ConfigPersister, EitherFormOrJson, ResponseFormat};

use crate::{ExperimentConfig, ExperimentList};
use templates::{ExperimentEditPage, ExperimentsPage};
use views::ExperimentRow;

#[derive(Clone)]
struct AdminState {
    experiments: ExperimentList,
    persister: Arc<dyn ConfigPersister>,
}

/// Build the admin router for experiments. Cli merges this into the
/// combined `/admin` router.
pub fn router(experiments: ExperimentList, persister: Arc<dyn ConfigPersister>) -> Router {
    let state = AdminState {
        experiments,
        persister,
    };
    Router::new()
        .route("/experiments", get(list).post(create))
        .route("/experiments/new", get(new_form))
        .route(
            "/experiments/{name}",
            get(detail).put(update).delete(remove),
        )
        .route("/experiments/{name}/edit", get(edit_form))
        .with_state(state)
}

async fn list(
    State(state): State<AdminState>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let snapshot = state.experiments.load();
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(&*snapshot.clone()).into_response());
    }
    let mut rows: Vec<ExperimentRow> = snapshot.iter().map(ExperimentRow::build).collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Html(ExperimentsPage { experiments: rows }.render()?).into_response())
}

async fn detail(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let snapshot = state.experiments.load();
    let config = snapshot
        .iter()
        .find(|e| e.name == name)
        .ok_or(AdminError::NotFound)?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(config.clone()).into_response());
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
    {
        let snapshot = state.experiments.load();
        if snapshot.iter().any(|e| e.name == experiment.name) {
            return Err(AdminError::Conflict(format!(
                "experiment '{}' already exists",
                experiment.name
            )));
        }
    }
    let mut updated: Vec<ExperimentConfig> = state.experiments.load().as_ref().clone();
    updated.push(experiment.clone());
    persist(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok((StatusCode::CREATED, Json(experiment)).into_response());
    }
    redirect("/admin/experiments")
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
    let mut updated: Vec<ExperimentConfig> = state.experiments.load().as_ref().clone();
    let slot = updated
        .iter_mut()
        .find(|e| e.name == name)
        .ok_or(AdminError::NotFound)?;
    *slot = experiment.clone();
    persist(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(experiment).into_response());
    }
    redirect("/admin/experiments")
}

async fn remove(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let mut updated: Vec<ExperimentConfig> = state.experiments.load().as_ref().clone();
    let before = updated.len();
    updated.retain(|e| e.name != name);
    if updated.len() == before {
        return Err(AdminError::NotFound);
    }
    persist(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    redirect("/admin/experiments")
}

async fn edit_form(
    State(state): State<AdminState>,
    Path(name): Path<String>,
) -> Result<Response, AdminError> {
    let snapshot = state.experiments.load();
    let config = snapshot
        .iter()
        .find(|e| e.name == name)
        .ok_or(AdminError::NotFound)?;
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

async fn persist(state: &AdminState, experiments: Vec<ExperimentConfig>) -> Result<(), AdminError> {
    let value =
        serde_yaml::to_value(&experiments).map_err(|err| AdminError::Internal(err.to_string()))?;
    state
        .persister
        .write_section("experiments", value)
        .await
        .map_err(AdminError::from)
}

fn redirect(to: &str) -> Result<Response, AdminError> {
    let mut resp = (StatusCode::SEE_OTHER, [("location", to)]).into_response();
    resp.headers_mut().insert(
        "hx-redirect",
        axum::http::HeaderValue::from_str(to).expect("valid header value"),
    );
    Ok(resp)
}

#[derive(Debug)]
enum AdminError {
    BadRequest(String),
    Conflict(String),
    Internal(String),
    InvalidConfig(String),
    NotFound,
    Render(askama::Error),
}

impl From<askama::Error> for AdminError {
    fn from(err: askama::Error) -> Self {
        Self::Render(err)
    }
}

impl From<ConfigPersistError> for AdminError {
    fn from(err: ConfigPersistError) -> Self {
        match err {
            ConfigPersistError::Invalid(m) | ConfigPersistError::Parse(m) => Self::InvalidConfig(m),
            ConfigPersistError::Io(m) => Self::Internal(m),
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Conflict(m) => (StatusCode::CONFLICT, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            Self::InvalidConfig(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
            Self::NotFound => (StatusCode::NOT_FOUND, "experiment not found".to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, msg).into_response()
    }
}
