use std::sync::Arc;

use experiments::ExperimentConfig;
use judge::Judges;
use memory::Store;
use telemetry::Sink as TelemetrySink;

use crate::auth::StudioAuth;

/// Shared state for the studio UI. Held in an `Arc` so axum handlers can
/// cheaply clone the reference. The studio reads directly from each
/// feature's storage layer — `memory` for messages and facts, `judge`
/// for scores, `telemetry` for the per-turn event tree — exactly as the
/// chat handler writes them. There is no second store to keep in sync.
pub struct StudioState {
    pub auth: Option<StudioAuth>,
    /// A/B experiments declared in `coulisse.yaml`. The studio renders
    /// these as a static config view; per-variant metrics arrive when the
    /// shadow/bandit strategies land and start producing comparable scores.
    pub experiments: Vec<ExperimentConfig>,
    pub judges: Arc<Judges>,
    pub memory: Arc<Store>,
    pub telemetry: Arc<TelemetrySink>,
}
