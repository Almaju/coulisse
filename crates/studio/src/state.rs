use std::sync::Arc;

use experiments::ExperimentConfig;
use memory::Store;
use telemetry::Sink as TelemetrySink;

use crate::auth::StudioAuth;

/// Shared state for the studio UI. Held in an `Arc` so axum handlers can
/// cheaply clone the reference. The studio reads from `memory` and
/// `telemetry` exactly as the proxy writes to them — there's no second
/// store to keep in sync.
pub struct StudioState {
    pub auth: Option<StudioAuth>,
    /// A/B experiments declared in `coulisse.yaml`. The studio renders
    /// these as a static config view; per-variant metrics arrive when the
    /// shadow/bandit strategies land and start producing comparable scores.
    pub experiments: Vec<ExperimentConfig>,
    pub memory: Arc<Store>,
    pub telemetry: Arc<TelemetrySink>,
}
