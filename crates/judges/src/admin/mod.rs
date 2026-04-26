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
use std::time::{SystemTime, UNIX_EPOCH};

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use coulisse_core::UserId;
use serde::Deserialize;
use uuid::Uuid;

use crate::{JudgeConfig, JudgeStoreError, Judges};
use templates::{JudgeDetailPage, JudgesPage, ScoresFragment, ScoresMeansFragment};
use views::{JudgeDetailRow, JudgeListRow, ScoreRow, ScoreRowMean, ScoresPanel, build_matrix};

#[derive(Clone)]
struct JudgesAdminState {
    configs: Arc<Vec<JudgeConfig>>,
    store: Arc<Judges>,
}

/// Build the admin router for judges. Cli merges this into the combined
/// `/admin` router.
pub fn router(judges: Arc<Judges>, configs: Arc<Vec<JudgeConfig>>) -> Router {
    Router::new()
        .route("/agents/{name}/scores", get(agent_scores))
        .route("/judges", get(judges_page))
        .route("/judges/{name}", get(judge_detail))
        .route("/scores/means", get(scores_means))
        .route("/users/{user_id}/scores", get(user_scores))
        .with_state(JudgesAdminState {
            configs,
            store: judges,
        })
}

async fn agent_scores(
    State(state): State<JudgesAdminState>,
    Path(name): Path<String>,
) -> Result<Html<String>, AdminError> {
    let scores = state.store.scores_for_agent(&name).await?;
    let panel = ScoresPanel::build(scores);
    render(ScoresFragment { scores: panel })
}

async fn judge_detail(
    State(state): State<JudgesAdminState>,
    Path(name): Path<String>,
) -> Result<Html<String>, AdminError> {
    let config = state
        .configs
        .iter()
        .find(|c| c.name == name)
        .ok_or(AdminError::NotFound)?;
    let since = now_secs().saturating_sub(7 * 86_400);
    let matrix_cells = state.store.agent_criterion_matrix(&name, since).await?;
    let recent = state.store.scores_for_judge(&name, 20).await?;
    let recent_scores: Vec<ScoreRow> = recent.into_iter().map(ScoreRow::from_score).collect();
    render(JudgeDetailPage {
        judge: JudgeDetailRow::from_config(config),
        matrix: build_matrix(matrix_cells),
        recent_scores,
    })
}

async fn judges_page(State(state): State<JudgesAdminState>) -> Result<Html<String>, AdminError> {
    let since = now_secs().saturating_sub(7 * 86_400);
    let volumes = state.store.score_volume(since).await?;
    let judges: Vec<JudgeListRow> = state
        .configs
        .iter()
        .map(|c| {
            let score_count_7d = volumes
                .iter()
                .find(|v| v.judge_name == c.name)
                .map(|v| v.count)
                .unwrap_or(0);
            JudgeListRow {
                criteria_count: c.rubrics.len(),
                model: c.model.clone(),
                name: c.name.clone(),
                provider: c.provider.clone(),
                sampling_rate: format!("{:.0}%", c.sampling_rate * 100.0),
                score_count_7d,
            }
        })
        .collect();
    render(JudgesPage { judges })
}

async fn user_scores(
    State(state): State<JudgesAdminState>,
    Path(user_id): Path<String>,
) -> Result<Html<String>, AdminError> {
    let user_id = parse_user_id(&user_id)?;
    let panel = ScoresPanel::build(state.store.scores(user_id).await?);
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
    State(state): State<JudgesAdminState>,
    Query(q): Query<MeansQuery>,
) -> Result<Html<String>, AdminError> {
    let since = q.since.unwrap_or(0);
    let scores = state
        .store
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug)]
enum AdminError {
    InvalidUserId,
    Judge(JudgeStoreError),
    NotFound,
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
            Self::NotFound => (StatusCode::NOT_FOUND, "judge not found".to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, message).into_response()
    }
}
