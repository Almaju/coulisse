//! YAML schema and validation for `coulisse.yaml`.
//!
//! Owns the deploy-time config shape (providers, agents, judges, mcp,
//! memory, studio auth) plus its validation.

mod error;
mod schema;
mod validate;

pub use error::ConfigError;
pub use schema::{
    AgentConfig, Config, JudgeConfig, McpServerConfig, McpToolAccess, ProviderConfig, ProviderKind,
    StudioBasicConfig, StudioConfig, StudioOidcConfig,
};
