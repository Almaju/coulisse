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
use coulisse_core::{EitherFormOrJson, ResponseFormat, ScoreQuery, UserId, now_secs, redirect_to};
use serde::Deserialize;

use crate::merge::{AdminJudge, admin_view};
use crate::{JudgeConfig, JudgeList, JudgeStoreError, Judges};
use templates::{JudgeDetailPage, JudgeEditPage, JudgesPage, ScoresFragment, ScoresMeansFragment};
use views::{
    AgentCriterionMatrix, JudgeDetailRow, JudgeListRow, ScoreRow, ScoreRowMean, ScoresPanel,
};

/// Everything the judges admin routes read and write. Cli builds one and
/// mounts [`JudgesAdmin::router`] under `/admin`.
#[derive(Clone)]
pub struct JudgesAdmin {
    /// Effective merged list (DB shadows + YAML). Updated atomically by
    /// `Judges::rebuild_judges` after every write.
    pub runtime_configs: JudgeList,
    pub store: Arc<Judges>,
    /// Raw YAML view. Used to compute admin row source labels and to
    /// decide tombstone-vs-delete on the smart `DELETE` endpoint.
    pub yaml_configs: JudgeList,
}

impl JudgesAdmin {
    pub fn router(self) -> Router {
        Router::new()
            .route("/agents/{name}/scores", get(Self::agent_scores))
            .route("/judges", get(Self::judges_page).post(Self::create_judge))
            .route("/judges/new", get(|| async { Self::new_form() }))
            .route(
                "/judges/{name}",
                get(Self::judge_detail)
                    .put(Self::update_judge)
                    .delete(Self::remove_judge),
            )
            .route("/judges/{name}/edit", get(Self::edit_form))
            .route("/judges/{name}/reset", post(Self::reset_judge))
            .route("/scores/means", get(Self::scores_means))
            .route("/users/{user_id}/scores", get(Self::user_scores))
            .with_state(self)
    }

    async fn agent_scores(
        State(state): State<Self>,
        Path(name): Path<String>,
    ) -> Result<Html<String>, AdminError> {
        let scores = state.store.scores_for_agent(&name).await?;
        let panel = ScoresPanel::build(scores);
        render(ScoresFragment { scores: panel })
    }

    async fn create_judge(
        State(state): State<Self>,
        fmt: ResponseFormat,
        EitherFormOrJson(judge): EitherFormOrJson<JudgeConfig>,
    ) -> Result<Response, AdminError> {
        state.store.put_active_dynamic(&judge.name, &judge).await?;
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok((StatusCode::CREATED, Json(judge)).into_response());
        }
        Ok(redirect_to(&format!("/admin/judges/{}", judge.name)))
    }

    async fn current_admin_view(&self) -> Result<Vec<AdminJudge>, AdminError> {
        let db = self.store.list_dynamic().await?;
        let yaml = self.yaml_configs.load();
        Ok(admin_view(&yaml, &db))
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
            AdminError::BadRequest("cannot edit a tombstoned judge — re-enable it first".into())
        })?;
        let yaml = serde_yaml::to_string(config)?;
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

    async fn judge_detail(
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
        let since = now_secs().saturating_sub(7 * 86_400);
        let matrix_cells = state.store.agent_criterion_matrix(&name, since).await?;
        let recent = state.store.scores_for_judge(&name, 20).await?;
        let recent_scores: Vec<ScoreRow> = recent.into_iter().map(ScoreRow::from_score).collect();
        Ok(Html(
            JudgeDetailPage {
                judge: JudgeDetailRow::from_admin(row),
                matrix: AgentCriterionMatrix::build(&matrix_cells),
                recent_scores,
            }
            .render()?,
        )
        .into_response())
    }

    async fn judges_page(
        State(state): State<Self>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let rows = state.current_admin_view().await?;
        if matches!(fmt, ResponseFormat::Json) {
            let configs: Vec<&JudgeConfig> =
                rows.iter().filter_map(|r| r.config.as_ref()).collect();
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

    fn new_form() -> Result<Response, AdminError> {
        let yaml =
            "name: \nprovider: openai\nmodel: \nsampling_rate: 1.0\nrubrics: {}\n".to_string();
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

    async fn rebuild(&self) -> Result<(), AdminError> {
        let yaml = self.yaml_configs.load_full();
        self.store
            .rebuild_judges(&self.runtime_configs, &yaml)
            .await?;
        Ok(())
    }

    async fn remove_judge(
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
        Ok(redirect_to("/admin/judges"))
    }

    async fn reset_judge(
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
        Ok(redirect_to(&format!("/admin/judges/{name}")))
    }

    async fn scores_means(
        State(state): State<Self>,
        Query(q): Query<MeansQuery>,
    ) -> Result<Html<String>, AdminError> {
        let scores = state
            .store
            .mean_scores_by_agent(ScoreQuery {
                criterion: &q.criterion,
                judge: &q.judge,
                since: q.since.unwrap_or(0),
            })
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

    async fn update_judge(
        State(state): State<Self>,
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
        state.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(judge).into_response());
        }
        Ok(redirect_to(&format!("/admin/judges/{name}")))
    }

    async fn user_scores(
        State(state): State<Self>,
        Path(user_id): Path<String>,
    ) -> Result<Html<String>, AdminError> {
        let user_id = user_id
            .parse::<UserId>()
            .map_err(AdminError::InvalidUserId)?;
        let panel = ScoresPanel::build(state.store.scores(user_id).await?);
        render(ScoresFragment { scores: panel })
    }
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

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

#[derive(Debug, thiserror::Error)]
enum AdminError {
    #[error("{0}")]
    BadRequest(String),
    #[error("user_id must be a valid UUID")]
    InvalidUserId(#[source] uuid::Error),
    #[error(transparent)]
    Judge(#[from] JudgeStoreError),
    #[error("judge not found")]
    NotFound,
    #[error("render failed: {0}")]
    Render(#[from] askama::Error),
    #[error("could not serialize judge config as YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::BadRequest(_) | Self::InvalidUserId(_) => StatusCode::BAD_REQUEST,
            Self::Judge(_) | Self::Render(_) | Self::Yaml(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::NotFound => StatusCode::NOT_FOUND,
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "judges admin request failed");
        }
        (status, self.to_string()).into_response()
    }
}
