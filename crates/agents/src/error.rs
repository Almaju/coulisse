use mcp::McpError;
use providers::{CallError, ClientInitError, ProviderKind};
use thiserror::Error;

/// Runtime errors raised after config has loaded successfully. Anything
/// that's a static schema/coverage failure lives in
/// `coulisse::config::ConfigError` (in cli) instead.
#[derive(Debug, Error)]
pub enum AgentsError {
    #[error(transparent)]
    ClientInit(#[from] ClientInitError),
    #[error(transparent)]
    Mcp(#[from] McpError),
    #[error(transparent)]
    Provider(#[from] CallError),
    #[error("agent '{agent}' references provider '{provider}' which is not configured")]
    ProviderNotConfigured {
        agent: String,
        provider: ProviderKind,
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
        Self::Provider(CallError::EmptyConversation)
    }
}
