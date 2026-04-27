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

#[derive(Clone)]
struct AdminState {
    /// Effective merged list (DB shadows + YAML). Updated atomically by
    /// `DynamicAgents::rebuild` after every write so the runtime hot
    /// path picks up admin edits immediately.
    runtime_agents: AgentList,
    store: Arc<DynamicAgents>,
    /// Raw YAML view, untouched by the DB. Used to compute admin row
    /// source labels and to decide tombstone-vs-delete on the smart
    /// `DELETE` endpoint.
    yaml_agents: AgentList,
}

pub fn router(
    runtime_agents: AgentList,
    store: Arc<DynamicAgents>,
    yaml_agents: AgentList,
) -> Router {
    let state = AdminState {
        runtime_agents,
        store,
        yaml_agents,
    };
    Router::new()
        .route("/agents", get(list).post(create))
        .route("/agents/new", get(new_form))
        .route(
            "/agents/{name}",
            get(detail).put(update).delete(remove_agent),
        )
        .route("/agents/{name}/edit", get(edit_form))
        .route("/agents/{name}/reset", post(reset))
        .with_state(state)
}

async fn list(
    State(state): State<AdminState>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let rows = current_admin_view(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        let configs: Vec<&AgentConfig> = rows.iter().filter_map(|r| r.config.as_ref()).collect();
        return Ok(Json(configs).into_response());
    }
    let view: Vec<AgentRow> = rows.iter().map(AgentRow::from_admin).collect();
    html(AgentsPage { agents: view })
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
    html(AgentDetailPage {
        agent: AgentDetailRow::from_admin(row),
    })
}

async fn create(
    State(state): State<AdminState>,
    fmt: ResponseFormat,
    EitherFormOrJson(agent): EitherFormOrJson<AgentConfig>,
) -> Result<Response, AdminError> {
    state.store.put_active(&agent.name, &agent).await?;
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok((StatusCode::CREATED, Json(agent)).into_response());
    }
    Ok(redirect_to(&format!("/admin/agents/{}", agent.name)))
}

async fn update(
    State(state): State<AdminState>,
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
    state.store.put_active(&name, &agent).await?;
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(agent).into_response());
    }
    Ok(redirect_to(&format!("/admin/agents/{name}")))
}

/// Smart delete. If YAML declares this name, write a tombstone (the YAML
/// entry is re-asserted on every load, so a physical delete would not stick).
/// Otherwise drop the row outright.
async fn remove_agent(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let yaml_backed = state.yaml_agents.load().iter().any(|c| c.name == name);
    let exists_in_db = state.store.list().await?.iter().any(|r| r.name == name);
    if !yaml_backed && !exists_in_db {
        return Err(AdminError::NotFound);
    }
    if yaml_backed {
        state.store.put_tombstone(&name).await?;
    } else {
        state.store.delete(&name).await?;
    }
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    Ok(redirect_to("/admin/agents"))
}

/// Drop the DB row outright. For an Override this lets YAML reassert; for
/// a Tombstoned-with-YAML this re-enables the YAML version; for a
/// Tombstoned-orphan this just cleans up. 404 when there is no DB row.
async fn reset(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let removed = state.store.delete(&name).await?;
    if !removed {
        return Err(AdminError::NotFound);
    }
    rebuild(&state).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    Ok(redirect_to(&format!("/admin/agents/{name}")))
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
        AdminError::BadRequest("cannot edit a tombstoned agent — re-enable it first".into())
    })?;
    let yaml =
        serde_yaml::to_string(config).map_err(|err| AdminError::Internal(err.to_string()))?;
    html(AgentEditPage {
        action: format!("/admin/agents/{name}"),
        is_new: false,
        method: "put",
        name,
        yaml,
    })
}

async fn new_form() -> Result<Response, AdminError> {
    let yaml = "name: \nprovider: openai\nmodel: \npreamble: \n".to_string();
    html(AgentEditPage {
        action: "/admin/agents".to_string(),
        is_new: true,
        method: "post",
        name: String::new(),
        yaml,
    })
}

async fn current_admin_view(state: &AdminState) -> Result<Vec<AdminAgent>, AdminError> {
    let db = state.store.list().await?;
    let yaml = state.yaml_agents.load();
    Ok(admin_view(&yaml, &db))
}

async fn rebuild(state: &AdminState) -> Result<(), AdminError> {
    let yaml = state.yaml_agents.load_full();
    state.store.rebuild(&state.runtime_agents, &yaml).await?;
    Ok(())
}

fn html<T: Template>(tpl: T) -> Result<Response, AdminError> {
    Ok(Html(tpl.render()?).into_response())
}

#[derive(Debug)]
enum AdminError {
    BadRequest(String),
    Internal(String),
    Invalid(String),
    NotFound,
    Render(askama::Error),
}

impl From<askama::Error> for AdminError {
    fn from(err: askama::Error) -> Self {
        Self::Render(err)
    }
}

impl From<DynamicAgentsError> for AdminError {
    fn from(err: DynamicAgentsError) -> Self {
        match err {
            DynamicAgentsError::Database(e) => Self::Internal(e.to_string()),
            DynamicAgentsError::Migrate(e) => Self::Internal(e.to_string()),
            DynamicAgentsError::RowDecode(m) | DynamicAgentsError::Serialize(m) => Self::Invalid(m),
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            Self::Invalid(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
            Self::NotFound => (StatusCode::NOT_FOUND, "agent not found".to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, msg).into_response()
    }
}
