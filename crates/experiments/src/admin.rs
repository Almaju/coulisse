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

/// Everything the experiments admin routes read and write. Cli builds
/// one and mounts [`ExperimentsAdmin::router`] under `/admin`.
#[derive(Clone)]
pub struct ExperimentsAdmin {
    pub runtime_experiments: ExperimentList,
    pub store: Arc<Experiments>,
    pub yaml_experiments: ExperimentList,
}

impl ExperimentsAdmin {
    pub fn router(self) -> Router {
        Router::new()
            .route("/experiments", get(Self::list).post(Self::create))
            .route("/experiments/new", get(|| async { Self::new_form() }))
            .route(
                "/experiments/{name}",
                get(Self::detail).put(Self::update).delete(Self::remove),
            )
            .route("/experiments/{name}/edit", get(Self::edit_form))
            .route("/experiments/{name}/reset", post(Self::reset))
            .with_state(self)
    }

    async fn create(
        State(state): State<Self>,
        fmt: ResponseFormat,
        EitherFormOrJson(experiment): EitherFormOrJson<ExperimentConfig>,
    ) -> Result<Response, AdminError> {
        state
            .store
            .put_active_dynamic(&experiment.name, &experiment)
            .await?;
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok((StatusCode::CREATED, Json(experiment)).into_response());
        }
        Ok(redirect_to("/admin/experiments"))
    }

    async fn current_admin_view(&self) -> Result<Vec<AdminExperiment>, AdminError> {
        let db = self.store.list_dynamic().await?;
        let yaml = self.yaml_experiments.load();
        Ok(admin_view(&yaml, &db))
    }

    async fn detail(
        State(state): State<Self>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let rows = state.current_admin_view().await?;
        let row = rows
            .iter()
            .find(|r| r.name == name)
            .ok_or(AdminError::NotFound)?;
        if matches!(fmt, ResponseFormat::Json) {
            return match &row.config {
                None => Err(AdminError::NotFound),
                Some(cfg) => Ok(Json(cfg.clone()).into_response()),
            };
        }
        Ok(redirect_to(&format!("/admin/experiments#{name}")))
    }

    async fn edit_form(
        State(state): State<Self>,
        Path(name): Path<String>,
    ) -> Result<Response, AdminError> {
        let rows = state.current_admin_view().await?;
        let row = rows
            .iter()
            .find(|r| r.name == name)
            .ok_or(AdminError::NotFound)?;
        let config = row.config.as_ref().ok_or_else(|| {
            AdminError::BadRequest(
                "cannot edit a tombstoned experiment — re-enable it first".into(),
            )
        })?;
        let yaml = serde_yaml::to_string(config)?;
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

    async fn list(State(state): State<Self>, fmt: ResponseFormat) -> Result<Response, AdminError> {
        let rows = state.current_admin_view().await?;
        if matches!(fmt, ResponseFormat::Json) {
            let configs: Vec<&ExperimentConfig> =
                rows.iter().filter_map(|r| r.config.as_ref()).collect();
            return Ok(Json(configs).into_response());
        }
        let view: Vec<ExperimentRow> = rows.iter().map(ExperimentRow::from_admin).collect();
        Ok(Html(ExperimentsPage { experiments: view }.render()?).into_response())
    }

    fn new_form() -> Result<Response, AdminError> {
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

    async fn rebuild(&self) -> Result<(), AdminError> {
        let yaml = self.yaml_experiments.load_full();
        self.store.rebuild(&self.runtime_experiments, &yaml).await?;
        Ok(())
    }

    async fn remove(
        State(state): State<Self>,
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
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        Ok(redirect_to("/admin/experiments"))
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
        Ok(redirect_to("/admin/experiments"))
    }

    async fn update(
        State(state): State<Self>,
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
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(experiment).into_response());
        }
        Ok(redirect_to("/admin/experiments"))
    }
}

#[derive(Debug, thiserror::Error)]
enum AdminError {
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Experiments(#[from] ExperimentsError),
    #[error("experiment not found")]
    NotFound,
    #[error("render failed: {0}")]
    Render(#[from] askama::Error),
    #[error("could not serialize experiment config as YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Experiments(_) | Self::Render(_) | Self::Yaml(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::NotFound => StatusCode::NOT_FOUND,
        };
        (status, self.to_string()).into_response()
    }
}
