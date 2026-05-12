// WHY: cli is the boot path + glue layer. Per CLAUDE.md, `unwrap`/
// `expect` are acceptable at startup, on Mutex poisoning (the holder
// already panicked, so propagating is right), and on serializing a
// known-good struct to JSON. These cases dominate cli.
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

//! Coulisse CLI: orchestrates every feature crate behind one `OpenAI`-
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
pub mod memory_resolve;
pub mod openapi;
pub mod paths;
pub mod server;
pub mod shadow;
pub mod smoke_runner;
pub mod stream;
