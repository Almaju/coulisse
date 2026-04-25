use config::ProviderKind;
use thiserror::Error;

/// Runtime errors raised after config has loaded successfully. Anything
/// that's a static schema/coverage failure lives in `config::ConfigError`
/// instead.
#[derive(Debug, Error)]
pub enum PrompterError {
    #[error("failed to initialize {provider} client: {source}")]
    ClientInit {
        provider: ProviderKind,
        #[source]
        source: rig::http_client::Error,
    },
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
    #[error("provider request failed: {0}")]
    Provider(#[from] rig::completion::PromptError),
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
    #[error("provider streaming failed: {0}")]
    Streaming(String),
    #[error("subagent hop limit exceeded ({limit}) invoking '{subagent}'")]
    SubagentDepthExceeded { limit: usize, subagent: String },
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
}
