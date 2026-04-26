use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("failed to connect to MCP server '{server}': {source}")]
    Connect {
        server: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("failed to list tools for MCP server '{server}': {source}")]
    ListTools {
        server: String,
        #[source]
        source: rmcp::ServiceError,
    },
    #[error("agent '{agent}' references MCP server '{server}' which is not configured")]
    ServerNotConfigured { agent: String, server: String },
    #[error("MCP server '{server}' does not expose tool '{tool}' (agent '{agent}')")]
    ToolNotFound {
        agent: String,
        server: String,
        tool: String,
    },
    #[error("failed to spawn MCP server '{server}': {source}")]
    Spawn {
        server: String,
        #[source]
        source: std::io::Error,
    },
}
