use thiserror::Error;

use crate::ProviderKind;

#[derive(Debug, Error)]
pub enum PrompterError {
    #[error("failed to initialize {provider} client: {source}")]
    ClientInit {
        provider: ProviderKind,
        #[source]
        source: rig::http_client::Error,
    },
    #[error("default_user_id must be non-empty when set")]
    BlankDefaultUserId,
    #[error("duplicate agent name in config: {0}")]
    DuplicateAgent(String),
    #[error("agent '{agent}' lists subagent '{subagent}' more than once")]
    DuplicateSubagent { agent: String, subagent: String },
    #[error("conversation has no user or assistant messages")]
    EmptyConversation,
    #[error("failed to connect to MCP server '{server}': {source}")]
    McpConnect {
        server: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("failed to list tools for MCP server '{server}': {source}")]
    McpListTools {
        server: String,
        #[source]
        source: rmcp::ServiceError,
    },
    #[error("agent '{agent}' references MCP server '{server}' which is not configured")]
    McpServerNotConfigured { agent: String, server: String },
    #[error("MCP server '{server}' does not expose tool '{tool}' (agent '{agent}')")]
    McpToolNotFound {
        agent: String,
        server: String,
        tool: String,
    },
    #[error("config must declare at least one agent")]
    NoAgents,
    #[error("failed to parse config: {0}")]
    ParseConfig(serde_yaml::Error),
    #[error("provider request failed: {0}")]
    Provider(#[from] rig::completion::PromptError),
    #[error("provider streaming failed: {0}")]
    Streaming(String),
    #[error("agent '{agent}' references provider '{provider}' which is not configured")]
    ProviderNotConfigured {
        agent: String,
        provider: ProviderKind,
    },
    #[error("failed to read config file {path}: {source}")]
    ReadConfig {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to spawn MCP server '{server}': {source}")]
    SpawnMcp {
        server: String,
        #[source]
        source: std::io::Error,
    },
    #[error("agent '{0}' cannot list itself as a subagent")]
    SelfSubagent(String),
    #[error("subagent hop limit exceeded ({limit}) invoking '{subagent}'")]
    SubagentDepthExceeded { limit: usize, subagent: String },
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    #[error("agent '{agent}' references subagent '{subagent}' which is not defined")]
    UnknownSubagent { agent: String, subagent: String },
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
