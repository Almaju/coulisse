mod backend;
mod config;
mod error;
pub mod testing;

pub use backend::{Agents, BootConfig, RigAgents};
pub use config::AgentConfig;
pub use error::AgentsError;
pub use experiments::{ExperimentRouter, Resolved};
pub use providers::{
    Completion, CompletionStream, Message, Role, StreamEvent, ToolCallKind, Usage,
};
