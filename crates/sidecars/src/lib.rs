//! Long-lived helper processes Coulisse spawns alongside itself.
//!
//! A sidecar is *not* a feature of the agent runtime — it's a process the
//! user wants kept alive in lockstep with Coulisse. The canonical example
//! is a chat-platform bridge (`examples/matrix-bridge/bridge.py`) that
//! turns Matrix mentions into webhook POSTs: it has nothing to do with
//! agents, but if you have to launch it in a separate terminal every
//! time, you've lost the "one YAML, one start command" property.
//!
//! Coulisse stays platform-agnostic. The sidecars mechanism only knows
//! how to spawn a command, capture its output, and restart it on crash.
//! It doesn't know what Matrix is.

mod config;
mod supervisor;

pub use config::{RestartPolicy, SidecarConfig};
pub use supervisor::spawn_all;
