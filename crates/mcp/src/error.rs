use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("cached OAuth client metadata for server '{server}' is not valid JSON: {source}")]
    ClientMetadata {
        server: String,
        #[source]
        source: serde_json::Error,
    },
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
    Decrypt { err: aes_gcm::Error, server: String },
    #[error("decrypted token for server '{server}' is not valid UTF-8: {source}")]
    DecryptedNotUtf8 {
        server: String,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("failed to fetch OAuth metadata from {url}: {source}")]
    Discovery {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
        url: String,
    },
    #[error(
        "malformed MCP server URL '{url}' (cannot derive origin for OAuth discovery): {source}"
    )]
    DiscoveryInvalidUrl {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
        url: String,
    },
    #[error("OAuth discovery at {url} returned HTTP {status}")]
    DiscoveryStatus { status: u16, url: String },
    #[error("Dynamic Client Registration failed for server '{server}': {source}")]
    DynamicClientRegistration {
        server: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("failed to encrypt token for server '{server}': {err}")]
    Encrypt { err: aes_gcm::Error, server: String },
    #[error("failed to list tools for MCP server '{server}': {source}")]
    ListTools {
        server: String,
        #[source]
        source: Box<rmcp::ServiceError>,
    },
    #[error("user '{user_id}' has not connected their '{server}' account")]
    NotConnected { server: String, user_id: String },
    #[error("MCP server '{server}' has no oauth block configured")]
    OAuthNotConfigured { server: String },
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
    #[error("state token has expired")]
    StateExpired,
    #[error("invalid state token: {source}")]
    StateInvalid {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error(
        "hmac key is invalid base64 or wrong length (must be 32 bytes base64-encoded): {source}"
    )]
    StateKeyInvalid {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("token exchange failed for server '{server}': {source}")]
    TokenExchange {
        server: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("MCP server '{server}' does not expose tool '{tool}' (agent '{agent}')")]
    ToolNotFound {
        agent: String,
        server: String,
        tool: String,
    },
    #[error(
        "vault key is invalid base64 or wrong length (must be 32 bytes base64-encoded): {source}"
    )]
    VaultKeyInvalid {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl McpError {
    /// Detect "the MCP endpoint refused this token" by string-matching the
    /// rmcp error chain. Strict downcasting would need to thread the inner
    /// transport error type (`StreamableHttpError<reqwest::Error>`) through
    /// `Box<dyn Error>` boundaries that rmcp doesn't expose. The variants
    /// are stable, public API; if they ever change, the unit tests fail
    /// before users notice.
    ///
    /// We match three shapes because real-world MCP endpoints don't all
    /// return clean RFC-9728 `WWW-Authenticate` headers on 401:
    ///
    /// - `AuthRequired(...)` — rmcp parsed a proper Bearer challenge.
    /// - `InsufficientScope(...)` — 403 with `scope=` in the challenge.
    /// - `UnexpectedServerResponse("HTTP 401 ...")` / `("HTTP 403 ...")` —
    ///   server returned 401/403 without a parseable challenge (Atlassian's
    ///   MCP at `mcp.atlassian.com` does this).
    #[must_use]
    pub(crate) fn looks_like_auth_failure(&self) -> bool {
        let formatted = format!("{self:?}");
        formatted.contains("AuthRequired")
            || formatted.contains("InsufficientScope")
            || formatted.contains("HTTP 401")
            || formatted.contains("HTTP 403")
    }
}
