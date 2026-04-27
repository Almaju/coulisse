//! Admin/studio HTTP surface for the judges crate. Exposes per-user score
//! panels (loaded into the conversation sidebar via htmx), per-(judge,
//! criterion) bandit summaries, and CRUD over the judge configs.
//!
//! Judge writes go to the `dynamic_judges` table, never to `coulisse.yaml`.
//! Resolution is "DB wins, YAML fallback" — see `merge` for the full rule.

mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use coulisse_core::{EitherFormOrJson, ResponseFormat, UserId, now_secs, redirect_to};
use serde::Deserialize;
use uuid::Uuid;

use crate::merge::{AdminJudge, admin_view};
use crate::{JudgeConfig, JudgeList, JudgeStoreError, Judges};
use templates::{JudgeDetailPage, JudgeEditPage, JudgesPage, ScoresFragment, ScoresMeansFragment};
use views::{JudgeDetailRow, JudgeListRow, ScoreRow, ScoreRowMean, ScoresPanel, build_matrix};

#[derive(Clone)]
struct JudgesAdminState {
    /// Effective merged list (DB shadows + YAML). Updated atomically by
    /// `Judges::rebuild_judges` after every write.
    runtime_configs: JudgeList,
    store: Arc<Judges>,
    /// Raw YAML view. Used to compute admin row source labels and to
    /// decide tombstone-vs-delete on the smart `DELETE` endpoint.
    yaml_configs: JudgeList,
}

pub fn router(judges: Arc<Judges>, runtime_configs: JudgeList, yaml_configs: JudgeList) -> Router {
    Router::new()
        .route("/agents/{name}/scores", get(agent_scores))
        .route("/judges", get(judges_page).post(create_judge))
        .route("/judges/new", get(new_form))
        .route(
            "/judges/{name}",
            get(judge_detail).put(update_judge).delete(remove_judge),
        )
        .route("/judges/{name}/edit", get(edit_form))
        .route("/judges/{name}/reset", post(reset_judge))
        .route("/scores/means", get(scores_means))
        .route("/users/{user_id}/scores", get(user_scores))
        .with_state(JudgesAdminState {
            runtime_configs,
            store: judges,
            yaml_configs,
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

async fn judges_page(
    State(state): State<JudgesAdminState>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let rows = current_admin_view(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        let configs: Vec<&JudgeConfig> = rows.iter().filter_map(|r| r.config.as_ref()).collect();
        return Ok(Json(configs).into_response());
    }
    let since = now_secs().saturating_sub(7 * 86_400);
    let volumes = state.store.score_volume(since).await?;
    let view: Vec<JudgeListRow> = rows
        .iter()
        .map(|r| JudgeListRow::from_admin(r, &volumes))
        .collect();
    Ok(Html(JudgesPage { judges: view }.render()?).into_response())
}

async fn judge_detail(
    State(state): State<JudgesAdminState>,
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
    let since = now_secs().saturating_sub(7 * 86_400);
    let matrix_cells = state.store.agent_criterion_matrix(&name, since).await?;
    let recent = state.store.scores_for_judge(&name, 20).await?;
    let recent_scores: Vec<ScoreRow> = recent.into_iter().map(ScoreRow::from_score).collect();
    Ok(Html(
        JudgeDetailPage {
            judge: JudgeDetailRow::from_admin(row),
            matrix: build_matrix(&matrix_cells),
            recent_scores,
        }
        .render()?,
    )
    .into_response())
}

async fn create_judge(
    State(state): State<JudgesAdminState>,
    fmt: ResponseFormat,
    EitherFormOrJson(judge): EitherFormOrJson<JudgeConfig>,
) -> Result<Response, AdminError> {
    state.store.put_active_dynamic(&judge.name, &judge).await?;
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok((StatusCode::CREATED, Json(judge)).into_response());
    }
    Ok(redirect_to(&format!("/admin/judges/{}", judge.name)))
}

async fn update_judge(
    State(state): State<JudgesAdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
    EitherFormOrJson(judge): EitherFormOrJson<JudgeConfig>,
) -> Result<Response, AdminError> {
    if judge.name != name {
        return Err(AdminError::BadRequest(format!(
            "URL judge name '{name}' does not match body name '{}'",
            judge.name
        )));
    }
    state.store.put_active_dynamic(&name, &judge).await?;
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(judge).into_response());
    }
    Ok(redirect_to(&format!("/admin/judges/{name}")))
}

async fn remove_judge(
    State(state): State<JudgesAdminState>,
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
    Ok(redirect_to("/admin/judges"))
}

async fn reset_judge(
    State(state): State<JudgesAdminState>,
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
    Ok(redirect_to(&format!("/admin/judges/{name}")))
}

async fn edit_form(
    State(state): State<JudgesAdminState>,
    Path(name): Path<String>,
) -> Result<Response, AdminError> {
    let rows = current_admin_view(&state).await?;
    let row = rows
        .iter()
        .find(|r| r.name == name)
        .ok_or(AdminError::NotFound)?;
    let config = row.config.as_ref().ok_or_else(|| {
        AdminError::BadRequest("cannot edit a tombstoned judge — re-enable it first".into())
    })?;
    let yaml =
        serde_yaml::to_string(config).map_err(|err| AdminError::Internal(err.to_string()))?;
    Ok(Html(
        JudgeEditPage {
            action: format!("/admin/judges/{name}"),
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
    let yaml = "name: \nprovider: openai\nmodel: \nsampling_rate: 1.0\nrubrics: {}\n".to_string();
    Ok(Html(
        JudgeEditPage {
            action: "/admin/judges".to_string(),
            is_new: true,
            method: "post",
            name: String::new(),
            yaml,
        }
        .render()?,
    )
    .into_response())
}

async fn current_admin_view(state: &JudgesAdminState) -> Result<Vec<AdminJudge>, AdminError> {
    let db = state.store.list_dynamic().await?;
    let yaml = state.yaml_configs.load();
    Ok(admin_view(&yaml, &db))
}

async fn rebuild(state: &JudgesAdminState) -> Result<(), AdminError> {
    let yaml = state.yaml_configs.load_full();
    state
        .store
        .rebuild_judges(&state.runtime_configs, &yaml)
        .await?;
    Ok(())
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

#[derive(Debug)]
enum AdminError {
    BadRequest(String),
    Internal(String),
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
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
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
