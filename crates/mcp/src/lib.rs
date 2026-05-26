//! MCP (Model Context Protocol) server pool. Connects to every server
//! declared under `mcp:` in YAML at boot, lists their tools, and hands
//! rig-shaped `ToolDyn` instances back to whichever crate runs LLM agents.
//!
//! Like `providers`, this crate is an interface to the outside world (MCP
//! servers — separate processes or HTTP endpoints), not a feature in the
//! same sense as `memory` or `judges`. `agents` depends on it directly.
//!
//! OAuth-enabled servers keep their per-user sessions in `UserMcpPool`
//! (backed by a moka LRU cache); static-token servers share one long-lived
//! connection from `McpServers`.

mod config;
mod error;
pub mod oauth;
mod pool;
pub mod routes;
mod sanitize;
mod server;
pub mod vault;

pub use config::{McpOAuthConfig, McpServerConfig, McpToolAccess, McpTransport};
pub use error::McpError;
pub use pool::UserMcpPool;
pub use routes::{OAuthRouterState, router as oauth_router};
pub use server::McpServers;
pub use vault::{McpMigrator, StoredToken, TokenVault, VaultMigrator};
