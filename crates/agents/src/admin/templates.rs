use askama::Template;

use super::views::{AgentDetailRow, AgentRow};

#[derive(Template)]
#[template(path = "agent_detail.html")]
pub struct AgentDetailPage {
    pub agent: AgentDetailRow,
}

#[derive(Template)]
#[template(path = "agent_edit.html")]
pub struct AgentEditPage {
    pub action: String,
    pub is_new: bool,
    pub method: &'static str,
    pub name: String,
    pub yaml: String,
}

#[derive(Template)]
#[template(path = "agents.html")]
pub struct AgentsPage {
    pub agents: Vec<AgentRow>,
}
