use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("failed to connect to MCP server '{server}': {source}")]
    Connect {
        server: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error(
        "Coulisse is not configured with a public_base_url, and Dynamic Client Registration \
         for MCP server '{server}' needs one. Set `public_base_url:` at the top level of \
         coulisse.yaml (e.g. `http://localhost:8421` for local use, or your deployed origin)."
    )]
    DcrMissingBaseUrl { server: String },
    #[error(
        "MCP server '{server}' uses oauth: discover, but its authorization metadata \
         omits the registration_endpoint required for Dynamic Client Registration. \
         Switch this server's YAML to oauth: static with pre-registered credentials."
    )]
    DcrUnsupported { server: String },
    #[error("failed to decrypt token for server '{server}': {err}")]
    Decrypt { server: String, err: aes_gcm::Error },
    #[error("failed to fetch OAuth metadata from {url}: {source}")]
    Discovery {
        url: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("malformed MCP server URL '{url}' (cannot derive origin for OAuth discovery)")]
    DiscoveryInvalidUrl { url: String },
    #[error("OAuth discovery at {url} returned HTTP {status}")]
    DiscoveryStatus { status: u16, url: String },
    #[error("Dynamic Client Registration failed for server '{server}': {source}")]
    DynamicClientRegistration {
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
    #[error("user '{user_id}' has not connected their '{server}' account")]
    NotConnected { server: String, user_id: String },
    #[error("agent '{agent}' references MCP server '{server}' which is not configured")]
    ServerNotConfigured { agent: String, server: String },
    #[error("failed to spawn MCP server '{server}': {source}")]
    Spawn {
        server: String,
        #[source]
        source: std::io::Error,
    },
    #[error("database error for MCP vault: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("invalid HMAC state token")]
    StateInvalid,
    #[error("state token has expired")]
    StateExpired,
    #[error("MCP server '{server}' does not expose tool '{tool}' (agent '{agent}')")]
    ToolNotFound {
        agent: String,
        server: String,
        tool: String,
    },
    #[error("token exchange failed for server '{server}': {source}")]
    TokenExchange {
        server: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("failed to encrypt token for server '{server}': {err}")]
    Encrypt { server: String, err: aes_gcm::Error },
    #[error("vault key is invalid base64 or wrong length (must be 32 bytes base64-encoded)")]
    VaultKeyInvalid,
}
