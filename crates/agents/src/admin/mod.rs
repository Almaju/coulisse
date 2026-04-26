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

use crate::{AgentConfig, AgentList};
use templates::{AgentDetailPage, AgentEditPage, AgentsPage};
use views::{AgentDetailRow, AgentRow};

#[derive(Clone)]
struct AdminState {
    agents: AgentList,
    persister: Arc<dyn ConfigPersister>,
}

pub fn router(agents: AgentList, persister: Arc<dyn ConfigPersister>) -> Router {
    let state = AdminState { agents, persister };
    Router::new()
        .route("/agents", get(list).post(create))
        .route("/agents/new", get(new_form))
        .route(
            "/agents/{name}",
            get(detail).put(update).delete(remove_agent),
        )
        .route("/agents/{name}/edit", get(edit_form))
        .with_state(state)
}

async fn list(
    State(state): State<AdminState>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let snapshot = state.agents.load();
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(&*snapshot.clone()).into_response());
    }
    let mut rows: Vec<AgentRow> = snapshot.iter().map(AgentRow::build).collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    html(AgentsPage { agents: rows })
}

async fn detail(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let snapshot = state.agents.load();
    let config = snapshot
        .iter()
        .find(|a| a.name == name)
        .ok_or(AdminError::NotFound)?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(config.clone()).into_response());
    }
    html(AgentDetailPage {
        agent: AgentDetailRow::build(config),
    })
}

async fn create(
    State(state): State<AdminState>,
    fmt: ResponseFormat,
    EitherFormOrJson(agent): EitherFormOrJson<AgentConfig>,
) -> Result<Response, AdminError> {
    {
        let snapshot = state.agents.load();
        if snapshot.iter().any(|a| a.name == agent.name) {
            return Err(AdminError::Conflict(format!(
                "agent '{}' already exists",
                agent.name
            )));
        }
    }
    let mut updated: Vec<AgentConfig> = state.agents.load().as_ref().clone();
    updated.push(agent.clone());
    persist(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok((StatusCode::CREATED, Json(agent)).into_response());
    }
    redirect(&format!("/admin/agents/{}", agent.name))
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
    let mut updated: Vec<AgentConfig> = state.agents.load().as_ref().clone();
    let slot = updated
        .iter_mut()
        .find(|a| a.name == name)
        .ok_or(AdminError::NotFound)?;
    *slot = agent.clone();
    persist(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(agent).into_response());
    }
    redirect(&format!("/admin/agents/{}", agent.name))
}

async fn remove_agent(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let mut updated: Vec<AgentConfig> = state.agents.load().as_ref().clone();
    let before = updated.len();
    updated.retain(|a| a.name != name);
    if updated.len() == before {
        return Err(AdminError::NotFound);
    }
    persist(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    redirect("/admin/agents")
}

async fn edit_form(
    State(state): State<AdminState>,
    Path(name): Path<String>,
) -> Result<Response, AdminError> {
    let snapshot = state.agents.load();
    let config = snapshot
        .iter()
        .find(|a| a.name == name)
        .ok_or(AdminError::NotFound)?;
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

async fn persist(state: &AdminState, agents: Vec<AgentConfig>) -> Result<(), AdminError> {
    let value =
        serde_yaml::to_value(&agents).map_err(|err| AdminError::Internal(err.to_string()))?;
    state
        .persister
        .write_section("agents", value)
        .await
        .map_err(AdminError::from)
}

fn html<T: Template>(tpl: T) -> Result<Response, AdminError> {
    Ok(Html(tpl.render()?).into_response())
}

fn redirect(to: &str) -> Result<Response, AdminError> {
    // Use `HX-Redirect` for htmx clients (full-page navigation in a
    // boosted UI) and a plain 303 for everyone else. Browsers without
    // htmx end up on the new URL via the standard redirect.
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
    Invalid(String),
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
            ConfigPersistError::Invalid(msg) | ConfigPersistError::Parse(msg) => Self::Invalid(msg),
            ConfigPersistError::Io(msg) => Self::Internal(msg),
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Conflict(m) => (StatusCode::CONFLICT, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            Self::Invalid(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
            Self::NotFound => (StatusCode::NOT_FOUND, "agent not found".to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, msg).into_response()
    }
}
