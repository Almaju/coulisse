use crate::AgentConfig;

pub struct AgentDetailRow {
    pub judges: Vec<String>,
    pub mcp_tools: Vec<McpToolRow>,
    pub model: String,
    pub name: String,
    pub preamble: String,
    pub provider: String,
    pub purpose: Option<String>,
    pub subagents: Vec<String>,
}

pub struct AgentRow {
    pub judge_count: usize,
    pub model: String,
    pub name: String,
    pub provider: String,
    pub purpose: Option<String>,
    pub subagent_count: usize,
    pub tool_count: usize,
}

pub struct McpToolRow {
    pub only: Option<String>,
    pub server: String,
}

impl AgentDetailRow {
    pub fn build(config: &AgentConfig) -> Self {
        let mut judges = config.judges.clone();
        judges.sort();

        let mut mcp_tools: Vec<McpToolRow> = config
            .mcp_tools
            .iter()
            .map(|t| McpToolRow {
                only: t.only.as_ref().map(|names| {
                    let mut sorted = names.clone();
                    sorted.sort();
                    sorted.join(", ")
                }),
                server: t.server.clone(),
            })
            .collect();
        mcp_tools.sort_by(|a, b| a.server.cmp(&b.server));

        let mut subagents = config.subagents.clone();
        subagents.sort();

        Self {
            judges,
            mcp_tools,
            model: config.model.clone(),
            name: config.name.clone(),
            preamble: config.preamble.clone(),
            provider: config.provider.as_str().to_string(),
            purpose: config.purpose.clone(),
            subagents,
        }
    }
}

impl AgentRow {
    pub fn build(config: &AgentConfig) -> Self {
        Self {
            judge_count: config.judges.len(),
            model: config.model.clone(),
            name: config.name.clone(),
            provider: config.provider.as_str().to_string(),
            purpose: config.purpose.clone(),
            subagent_count: config.subagents.len(),
            tool_count: config.mcp_tools.len(),
        }
    }
}
