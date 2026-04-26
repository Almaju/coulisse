//! Admin/studio HTTP surface for the experiments crate. One page that
//! renders the static A/B configuration. Per-experiment bandit metrics
//! load via htmx from the judges admin router (`/admin/scores/means`),
//! so this module never depends on `judges`.

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

use crate::ExperimentConfig;
use templates::ExperimentsPage;
use views::ExperimentRow;

/// Build the admin router for experiments. Cli merges this into the
/// combined `/admin` router.
pub fn router(experiments: Arc<Vec<ExperimentConfig>>) -> Router {
    Router::new()
        .route("/experiments", get(experiments_page))
        .with_state(experiments)
}

async fn experiments_page(
    State(experiments): State<Arc<Vec<ExperimentConfig>>>,
) -> Result<Html<String>, AdminError> {
    let mut rows: Vec<ExperimentRow> = experiments.iter().map(ExperimentRow::build).collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    render(ExperimentsPage { experiments: rows })
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

#[derive(Debug)]
enum AdminError {
    Render(askama::Error),
}

impl From<askama::Error> for AdminError {
    fn from(err: askama::Error) -> Self {
        Self::Render(err)
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        match self {
            Self::Render(err) => {
                (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        }
    }
}
