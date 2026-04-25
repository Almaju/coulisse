//! LLM-as-judge scoring for Coulisse. Each configured judge runs in a
//! background task after an assistant turn, samples at a configurable rate,
//! asks its backing model to score the reply against a rubric, and persists
//! one `Score` row per criterion.
//!
//! Judge owns its own SQLite table (`scores`); reads are exposed via the
//! `coulisse_core::ScoreLookup` trait so feature crates that need to
//! consume scores (`agents`, the chat handler in `cli`) don't take a
//! hard dep on `judge`.
//!
//! The user config describes *what* to evaluate; this crate builds the judge
//! preamble and forces a JSON response shape internally so rubrics stay free
//! of format/scale boilerplate.

mod config;
mod judge;
mod store;
mod types;

pub use config::JudgeConfig;
pub use judge::{Judge, JudgeBuildError, spawn_score};
pub use store::{JudgeStoreError, Judges};
pub use types::{Score, ScoreId};
