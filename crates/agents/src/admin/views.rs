use crate::AgentConfig;
use crate::merge::{AdminAgent, AdminSource};

pub(super) struct AgentDetailRow {
    pub judges: Vec<String>,
    pub mcp_tools: Vec<McpToolRow>,
    pub model: String,
    pub name: String,
    pub preamble: String,
    pub provider: String,
    pub purpose: Option<String>,
    pub source: SourceLabel,
    pub subagents: Vec<String>,
    /// True when YAML declares this name. Drives action buttons:
    /// `yaml_backed` overrides offer "Reset to YAML"; `yaml_backed`
    /// tombstones offer "Re-enable"; non-`yaml_backed` rows offer "Delete".
    pub yaml_backed: bool,
}

pub(super) struct AgentRow {
    pub judge_count: usize,
    pub model: String,
    pub name: String,
    pub provider: String,
    pub purpose: Option<String>,
    pub source: SourceLabel,
    pub subagent_count: usize,
    pub tombstoned: bool,
    pub tool_count: usize,
}

pub(super) struct McpToolRow {
    pub only: Option<String>,
    pub server: String,
}

/// Stringified source label used by templates. Lowercase, stable — used as
/// CSS class suffix and in copy.
pub(super) struct SourceLabel(pub &'static str);

impl SourceLabel {
    pub(super) fn from_admin(source: AdminSource) -> Self {
        Self(match source {
            AdminSource::Dynamic => "dynamic",
            AdminSource::Override => "override",
            AdminSource::Tombstoned => "tombstoned",
            AdminSource::Yaml => "yaml",
        })
    }

    pub(super) fn as_str(&self) -> &'static str {
        self.0
    }
}

impl AgentDetailRow {
    pub(super) fn from_admin(row: &AdminAgent) -> Self {
        let label = SourceLabel::from_admin(row.source);
        match &row.config {
            Some(cfg) => Self::from_config(cfg, label, row.yaml_backed),
            None => Self {
                judges: Vec::new(),
                mcp_tools: Vec::new(),
                model: String::new(),
                name: row.name.clone(),
                preamble: String::new(),
                provider: String::new(),
                purpose: None,
                source: label,
                subagents: Vec::new(),
                yaml_backed: row.yaml_backed,
            },
        }
    }

    fn from_config(config: &AgentConfig, source: SourceLabel, yaml_backed: bool) -> Self {
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
            source,
            subagents,
            yaml_backed,
        }
    }
}

impl AgentRow {
    pub(super) fn from_admin(row: &AdminAgent) -> Self {
        let label = SourceLabel::from_admin(row.source);
        match &row.config {
            Some(cfg) => Self {
                judge_count: cfg.judges.len(),
                model: cfg.model.clone(),
                name: cfg.name.clone(),
                provider: cfg.provider.as_str().to_string(),
                purpose: cfg.purpose.clone(),
                source: label,
                subagent_count: cfg.subagents.len(),
                tombstoned: false,
                tool_count: cfg.mcp_tools.len(),
            },
            None => Self {
                judge_count: 0,
                model: String::new(),
                name: row.name.clone(),
                provider: String::new(),
                purpose: None,
                source: label,
                subagent_count: 0,
                tombstoned: true,
                tool_count: 0,
            },
        }
    }
}
