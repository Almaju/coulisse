use askama::Template;

use super::views::{AgentDetailRow, AgentRow};

#[derive(Template)]
#[template(path = "agent_detail.html")]
pub struct AgentDetailPage {
    pub agent: AgentDetailRow,
}

#[derive(Template)]
#[template(path = "agents.html")]
pub struct AgentsPage {
    pub agents: Vec<AgentRow>,
}
