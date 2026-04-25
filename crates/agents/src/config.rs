use std::collections::HashMap;

use backends::ProviderKind;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct AgentConfig {
    /// Names of judges (defined at the top level under `judges:`) that should
    /// evaluate this agent's replies. Empty = no automatic evaluation.
    #[serde(default)]
    pub judges: Vec<String>,
    #[serde(default)]
    pub mcp_tools: Vec<McpToolAccess>,
    pub model: String,
    pub name: String,
    #[serde(default)]
    pub preamble: String,
    pub provider: ProviderKind,
    /// Short description used as the tool description when this agent is
    /// exposed to other agents via `subagents:`. If absent, the agent's
    /// `name` is used as a fallback — but clear prose here helps the caller
    /// LLM decide when to invoke this agent.
    #[serde(default)]
    pub purpose: Option<String>,
    /// Other agents exposed to this agent as tools. Names must match entries
    /// in the top-level `agents:` list. Self-reference is rejected; duplicate
    /// entries are rejected. Calling a subagent runs a fresh conversation
    /// against that agent's preamble + MCP tools; the subagent's final
    /// message is returned as the tool result.
    #[serde(default)]
    pub subagents: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpToolAccess {
    #[serde(default)]
    pub only: Option<Vec<String>>,
    pub server: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpServerConfig {
    Http {
        url: String,
    },
    Stdio {
        #[serde(default)]
        args: Vec<String>,
        command: String,
        #[serde(default)]
        env: HashMap<String, String>,
    },
}
