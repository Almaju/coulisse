//! Smoke tests for Coulisse: a synthetic-user persona drives a
//! conversation against an agent (or experiment), the existing judge
//! pipeline scores each assistant turn, and the resulting transcripts
//! plus scores surface in the admin UI.
//!
//! Smoke owns its YAML slice (`smoke_tests:`), its tables (`smoke_runs`,
//! `smoke_messages`), and its admin pages. The runner that actually
//! drives the loop lives in `cli` — like the chat handler, it
//! orchestrates the feature crates rather than hiding inside one of
//! them. This crate exposes `SmokeStore` (storage) and `RunDispatcher`
//! (the trait the cli implements to launch runs from the "Run now"
//! button).

pub mod admin;
mod config;
mod dispatcher;
mod merge;
mod store;
mod types;

pub use config::{PersonaConfig, SmokeList, SmokeTestConfig, smoke_list};
pub use dispatcher::{DispatchError, RunDispatcher};
pub use merge::{AdminSmoke, AdminSource, MergeReport, MergedSmoke, Source, admin_view, merge};
pub use store::{DynamicSmokeRow, SmokeStore, SmokeStoreError};
pub use types::{RunId, RunStatus, StoredMessage, StoredRun, TurnRole};
