//! Experiment routing. Resolves an addressable name (agent or
//! experiment) to a concrete agent for a given user.
//!
//! Sticky-by-user routing uses an FNV-1a hash of `(user_id, experiment
//! name)` mod the cumulative variant weights, so the same user always
//! lands on the same variant across restarts without any DB writes.
//! Adding or removing a variant reshuffles users — that's the price of
//! statelessness, and it's documented behaviour.

pub mod admin;
mod config;
mod resolver;
mod router;

pub use config::{ExperimentConfig, Strategy, Variant};
pub use resolver::ExperimentResolver;
pub use router::{
    BANDIT_DEFAULT_EPSILON, BANDIT_DEFAULT_MIN_SAMPLES, BANDIT_DEFAULT_WINDOW_SECONDS,
    ExperimentRouter, Resolved,
};
