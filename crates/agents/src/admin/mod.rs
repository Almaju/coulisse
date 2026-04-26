mod templates;
mod views;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

use crate::AgentConfig;
use templates::{AgentDetailPage, AgentsPage};
use views::{AgentDetailRow, AgentRow};

pub fn router(agents: Arc<Vec<AgentConfig>>) -> Router {
    Router::new()
        .route("/agents", get(agents_page))
        .route("/agents/{name}", get(agent_detail))
        .with_state(agents)
}

async fn agent_detail(
    State(agents): State<Arc<Vec<AgentConfig>>>,
    Path(name): Path<String>,
) -> Result<Html<String>, AdminError> {
    let config = agents
        .iter()
        .find(|a| a.name == name)
        .ok_or(AdminError::NotFound)?;
    let agent = AgentDetailRow::build(config);
    render(AgentDetailPage { agent })
}

async fn agents_page(
    State(agents): State<Arc<Vec<AgentConfig>>>,
) -> Result<Html<String>, AdminError> {
    let mut rows: Vec<AgentRow> = agents.iter().map(AgentRow::build).collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    render(AgentsPage { agents: rows })
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, AdminError> {
    Ok(Html(tpl.render()?))
}

#[derive(Debug)]
enum AdminError {
    NotFound,
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
            Self::NotFound => StatusCode::NOT_FOUND.into_response(),
            Self::Render(err) => {
                (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        }
    }
}
