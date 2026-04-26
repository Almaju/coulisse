//! MCP (Model Context Protocol) server pool. Connects to every server
//! declared under `mcp:` in YAML at boot, lists their tools, and hands
//! rig-shaped `ToolDyn` instances back to whichever crate runs LLM agents.
//!
//! Like `providers`, this crate is an interface to the outside world (MCP
//! servers — separate processes or HTTP endpoints), not a feature in the
//! same sense as `memory` or `judges`. `agents` depends on it directly.

mod config;
mod error;
mod server;

pub use config::{McpServerConfig, McpToolAccess};
pub use error::McpError;
pub use server::McpServers;
