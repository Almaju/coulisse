//! Long-lived helper processes Coulisse spawns alongside itself.
//!
//! A sidecar is *not* a feature of the agent runtime — it's a process the
//! user wants kept alive in lockstep with Coulisse. The canonical example
//! is a chat-platform bridge that turns inbound chat events into webhook
//! POSTs: it has nothing to do with agents, but if you have to launch it
//! in a separate terminal every time, you've lost the "one YAML, one
//! start command" property.
//!
//! Coulisse stays platform-agnostic. The sidecars mechanism only knows
//! how to spawn a command, capture its output, and restart it on crash.

mod config;
mod supervisor;

pub use config::{RestartPolicy, SidecarConfig};
pub use supervisor::spawn_all;
