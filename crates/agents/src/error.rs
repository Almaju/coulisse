use backends::{CallError, ClientInitError, ProviderKind};
use thiserror::Error;

/// Runtime errors raised after config has loaded successfully. Anything
/// that's a static schema/coverage failure lives in
/// `coulisse::config::ConfigError` (in cli) instead.
#[derive(Debug, Error)]
pub enum AgentsError {
    #[error(transparent)]
    Backend(#[from] CallError),
    #[error(transparent)]
    ClientInit(#[from] ClientInitError),
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
    #[error("agent '{agent}' references provider '{provider}' which is not configured")]
    ProviderNotConfigured {
        agent: String,
        provider: ProviderKind,
    },
    #[error("failed to spawn MCP server '{server}': {source}")]
    SpawnMcp {
        server: String,
        #[source]
        source: std::io::Error,
    },
    #[error("subagent hop limit exceeded ({limit}) invoking '{subagent}'")]
    SubagentDepthExceeded { limit: usize, subagent: String },
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
}

impl AgentsError {
    /// Helper for tests and corner-case sites that need to construct
    /// the empty-conversation case directly without going through Rig.
    pub fn empty_conversation() -> Self {
        Self::Backend(CallError::EmptyConversation)
    }
}
