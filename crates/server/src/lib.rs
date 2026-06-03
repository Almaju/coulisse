//! HTTP-server configuration and process wiring.
//!
//! Owns the `server:` slice of `coulisse.yaml` — the bind address, port,
//! tokio worker-thread count, and request body cap — plus the two things
//! that depend on those values before any feature crate runs: building the
//! multi-threaded tokio runtime and applying transport-level layers to the
//! composed router. It deliberately knows nothing about routes; `cli`
//! composes the application and hands the finished router here for the body
//! limit only.

mod config;

pub use config::{BindError, ServerConfig};
