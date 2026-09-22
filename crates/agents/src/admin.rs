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

use crate::merge::{AdminAgent, admin_view};
use crate::store::{DynamicAgents, DynamicAgentsError};
use crate::{AgentConfig, AgentList};
use templates::{AgentDetailPage, AgentEditPage, AgentsPage};
use views::{AgentDetailRow, AgentRow};

/// Admin surface for runtime-mutable agents. Cli builds one and mounts
/// `router()` under `/admin`.
#[derive(Clone)]
pub struct AgentsAdmin {
    pub dynamic_agents: Arc<DynamicAgents>,
    /// Effective merged list (DB shadows + YAML). Updated atomically by
    /// `DynamicAgents::rebuild` after every write so the runtime hot
    /// path picks up admin edits immediately.
    pub runtime_agents: AgentList,
    /// Raw YAML view, untouched by the DB. Used to compute admin row
    /// source labels and to decide tombstone-vs-delete on the smart
    /// `DELETE` endpoint.
    pub yaml_agents: AgentList,
}

impl AgentsAdmin {
    pub fn router(self) -> Router {
        Router::new()
            .route("/agents", get(Self::list).post(Self::create))
            .route("/agents/new", get(|| std::future::ready(Self::new_form())))
            .route(
                "/agents/{name}",
                get(Self::detail)
                    .put(Self::update)
                    .delete(Self::remove_agent),
            )
            .route("/agents/{name}/edit", get(Self::edit_form))
            .route("/agents/{name}/reset", post(Self::reset))
            .with_state(self)
    }

    async fn create(
        State(admin): State<Self>,
        fmt: ResponseFormat,
        EitherFormOrJson(agent): EitherFormOrJson<AgentConfig>,
    ) -> Result<Response, AdminError> {
        admin.dynamic_agents.put_active(&agent.name, &agent).await?;
        admin.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok((StatusCode::CREATED, Json(agent)).into_response());
        }
        Ok(redirect_to(&format!("/admin/agents/{}", agent.name)))
    }

    async fn current_admin_view(&self) -> Result<Vec<AdminAgent>, AdminError> {
        let db = self.dynamic_agents.list().await?;
        let yaml = self.yaml_agents.load();
        Ok(admin_view(&yaml, &db))
    }

    async fn detail(
        State(admin): State<Self>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let rows = admin.current_admin_view().await?;
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
        html(AgentDetailPage {
            agent: AgentDetailRow::from_admin(row),
        })
    }

    async fn edit_form(
        State(admin): State<Self>,
        Path(name): Path<String>,
    ) -> Result<Response, AdminError> {
        let rows = admin.current_admin_view().await?;
        let row = rows
            .iter()
            .find(|r| r.name == name)
            .ok_or(AdminError::NotFound)?;
        let config = row.config.as_ref().ok_or_else(|| {
            AdminError::BadRequest("cannot edit a tombstoned agent — re-enable it first".into())
        })?;
        let yaml = serde_yaml::to_string(config)?;
        html(AgentEditPage {
            action: format!("/admin/agents/{name}"),
            is_new: false,
            method: "put",
            name,
            yaml,
        })
    }

    async fn list(State(admin): State<Self>, fmt: ResponseFormat) -> Result<Response, AdminError> {
        let rows = admin.current_admin_view().await?;
        if matches!(fmt, ResponseFormat::Json) {
            let configs: Vec<&AgentConfig> =
                rows.iter().filter_map(|r| r.config.as_ref()).collect();
            return Ok(Json(configs).into_response());
        }
        let view: Vec<AgentRow> = rows.iter().map(AgentRow::from_admin).collect();
        html(AgentsPage { agents: view })
    }

    fn new_form() -> Result<Response, AdminError> {
        let yaml = "name: \nprovider: openai\nmodel: \npreamble: \n".to_string();
        html(AgentEditPage {
            action: "/admin/agents".to_string(),
            is_new: true,
            method: "post",
            name: String::new(),
            yaml,
        })
    }

    async fn rebuild(&self) -> Result<(), AdminError> {
        let yaml = self.yaml_agents.load_full();
        self.dynamic_agents
            .rebuild(&self.runtime_agents, &yaml)
            .await?;
        Ok(())
    }

    /// Smart delete. If YAML declares this name, write a tombstone (the YAML
    /// entry is re-asserted on every load, so a physical delete would not stick).
    /// Otherwise drop the row outright.
    async fn remove_agent(
        State(admin): State<Self>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let yaml_backed = admin.yaml_agents.load().iter().any(|c| c.name == name);
        let exists_in_db = admin
            .dynamic_agents
            .list()
            .await?
            .iter()
            .any(|r| r.name == name);
        if !yaml_backed && !exists_in_db {
            return Err(AdminError::NotFound);
        }
        if yaml_backed {
            admin.dynamic_agents.put_tombstone(&name).await?;
        } else {
            admin.dynamic_agents.delete(&name).await?;
        }
        admin.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        Ok(redirect_to("/admin/agents"))
    }

    /// Drop the DB row outright. For an Override this lets YAML reassert; for
    /// a Tombstoned-with-YAML this re-enables the YAML version; for a
    /// Tombstoned-orphan this just cleans up. 404 when there is no DB row.
    async fn reset(
        State(admin): State<Self>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let removed = admin.dynamic_agents.delete(&name).await?;
        if !removed {
            return Err(AdminError::NotFound);
        }
        admin.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        Ok(redirect_to(&format!("/admin/agents/{name}")))
    }

    async fn update(
        State(admin): State<Self>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
        EitherFormOrJson(agent): EitherFormOrJson<AgentConfig>,
    ) -> Result<Response, AdminError> {
        if agent.name != name {
            return Err(AdminError::BadRequest(format!(
                "URL agent name '{name}' does not match body name '{}'",
                agent.name
            )));
        }
        admin.dynamic_agents.put_active(&name, &agent).await?;
        admin.rebuild().await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(agent).into_response());
        }
        Ok(redirect_to(&format!("/admin/agents/{name}")))
    }
}

fn html<T: Template>(tpl: T) -> Result<Response, AdminError> {
    Ok(Html(tpl.render()?).into_response())
}

#[derive(Debug, thiserror::Error)]
enum AdminError {
    #[error("{0}")]
    BadRequest(String),
    #[error("agent not found")]
    NotFound,
    #[error("template rendering failed: {0}")]
    Render(#[from] askama::Error),
    #[error(transparent)]
    Store(#[from] DynamicAgentsError),
    #[error("failed to serialize agent as YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl AdminError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Render(_) | Self::Yaml(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Store(DynamicAgentsError::RowDecode(_) | DynamicAgentsError::Serialize(_)) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::Store(DynamicAgentsError::Database(_) | DynamicAgentsError::Migrate(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, "agents admin request failed");
        }
        (status, self.to_string()).into_response()
    }
}
