//! Admin/studio HTTP surface for the judges crate. Exposes per-user score
//! panels (loaded into the conversation sidebar via htmx) and per-(judge,
//! criterion) bandit summaries (queried by the experiments page for any
//! bandit-strategy experiment).
//!
//! No coupling to memory or experiments: the conversation panel is keyed
//! by `user_id`, and the bandit summary is keyed by query parameters
//! (`judge`, `criterion`, `since`). Callers pass IDs in URLs/query strings
//! and the judges crate looks them up in its own table.

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use coulisse_core::UserId;
use serde::Deserialize;
use uuid::Uuid;

use crate::{JudgeStoreError, Judges};
use templates::{ScoresFragment, ScoresMeansFragment};
use views::{ScoreRowMean, ScoresPanel};

/// Build the admin router for judges. Cli merges this into the combined
/// `/admin` router.
pub fn router(judges: Arc<Judges>) -> Router {
    Router::new()
        .route("/users/{user_id}/scores", get(user_scores))
        .route("/scores/means", get(scores_means))
        .with_state(judges)
}

async fn user_scores(
    State(judges): State<Arc<Judges>>,
    Path(user_id): Path<String>,
) -> Result<Html<String>, AdminError> {
    let user_id = parse_user_id(&user_id)?;
    let panel = ScoresPanel::build(judges.scores(user_id).await?);
    render(ScoresFragment { scores: panel })
}

#[derive(Deserialize)]
struct MeansQuery {
    criterion: String,
    judge: String,
    /// Unix-seconds lower bound. Older scores are excluded from the mean.
    /// Defaults to 0 (all-time) when absent.
    #[serde(default)]
    since: Option<u64>,
}

async fn scores_means(
    State(judges): State<Arc<Judges>>,
    Query(q): Query<MeansQuery>,
) -> Result<Html<String>, AdminError> {
    let since = q.since.unwrap_or(0);
    let scores = judges
        .mean_scores_by_agent(&q.judge, &q.criterion, since)
        .await?;
    let mut rows: Vec<ScoreRowMean> = scores
        .into_iter()
        .map(|s| ScoreRowMean {
            agent: s.agent_name,
            mean: format!("{:.2}", s.mean),
            samples: s.samples,
        })
        .collect();
    rows.sort_by(|a, b| a.agent.cmp(&b.agent));
    render(ScoresMeansFragment { rows })
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

fn parse_user_id(raw: &str) -> Result<UserId, AdminError> {
    Uuid::parse_str(raw)
        .map(UserId::from)
        .map_err(|_| AdminError::InvalidUserId)
}

#[derive(Debug)]
enum AdminError {
    InvalidUserId,
    Judge(JudgeStoreError),
    Render(askama::Error),
}

impl From<JudgeStoreError> for AdminError {
    fn from(err: JudgeStoreError) -> Self {
        Self::Judge(err)
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
            Self::InvalidUserId => (
                StatusCode::BAD_REQUEST,
                "user_id must be a valid UUID".to_string(),
            ),
            Self::Judge(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, message).into_response()
    }
}
