//! Coulisse CLI: orchestrates every feature crate behind one OpenAI-
//! compatible HTTP server. The chat handler in `server` reads
//! top-to-bottom as the request-flow spec — limits → context → route
//! → call → score, with off-path work spawned as background tasks.

pub mod admin;
pub mod admin_extras;
pub mod banner;
pub mod commands;
pub mod config;
pub mod config_store;
pub mod error;
pub mod openapi;
pub mod paths;
pub mod server;
pub mod shadow;
pub mod smoke_runner;
pub mod stream;
