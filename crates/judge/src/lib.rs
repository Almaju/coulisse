//! LLM-as-judge scoring for Coulisse. Each configured judge runs in a
//! background task after an assistant turn, samples at a configurable rate,
//! asks its backing model to score the reply against a rubric, and persists
//! one `Score` row per criterion.
//!
//! The user config describes *what* to evaluate; this crate builds the judge
//! preamble and forces a JSON response shape internally so rubrics stay free
//! of format/scale boilerplate.

mod judge;

pub use judge::{Judge, JudgeBuildError, spawn_score};
