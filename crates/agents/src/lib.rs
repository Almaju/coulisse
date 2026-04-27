pub mod admin;
mod config;
mod error;
mod merge;
mod runtime;
mod store;
pub mod testing;
mod tools;

pub use config::{AgentConfig, AgentList, agent_list};
pub use error::AgentsError;
pub use merge::{AdminAgent, AdminSource, MergeReport, MergedAgent, Source, admin_view, merge};
pub use providers::{
    Completion, CompletionStream, Message, Role, StreamEvent, ToolCallKind, Usage,
};
pub use runtime::{Agents, BootConfig, RigAgents};
pub use store::{DynamicAgents, DynamicAgentsError, DynamicRow};
