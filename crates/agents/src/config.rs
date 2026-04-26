use std::sync::Arc;

use arc_swap::ArcSwap;
use mcp::McpToolAccess;
use providers::ProviderKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentConfig {
    /// Names of judges (defined at the top level under `judges:`) that should
    /// evaluate this agent's replies. Empty = no automatic evaluation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judges: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_tools: Vec<McpToolAccess>,
    pub model: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preamble: String,
    pub provider: ProviderKind,
    /// Short description used as the tool description when this agent is
    /// exposed to other agents via `subagents:`. If absent, the agent's
    /// `name` is used as a fallback — but clear prose here helps the caller
    /// LLM decide when to invoke this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Other agents exposed to this agent as tools. Names must match entries
    /// in the top-level `agents:` list. Self-reference is rejected; duplicate
    /// entries are rejected. Calling a subagent runs a fresh conversation
    /// against that agent's preamble + MCP tools; the subagent's final
    /// message is returned as the tool result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<String>,
}

/// Hot-reloadable list of agent configs. The same handle is held by the
/// runtime (`RigAgents`), the admin router, and the cli's `ConfigStore`
/// reload callback — all three see the same atomic swap when the YAML
/// changes. `load_full` returns a cheap `Arc` clone; readers never block
/// writers, writers never block readers.
pub type AgentList = Arc<ArcSwap<Vec<AgentConfig>>>;

pub fn agent_list(initial: Vec<AgentConfig>) -> AgentList {
    Arc::new(ArcSwap::from_pointee(initial))
}
