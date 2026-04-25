use std::sync::Arc;

use memory::Store;
use telemetry::Sink as TelemetrySink;

use crate::auth::StudioAuth;

/// Shared state for the studio UI. Held in an `Arc` so axum handlers can
/// cheaply clone the reference. The studio reads from `memory` and
/// `telemetry` exactly as the proxy writes to them — there's no second
/// store to keep in sync.
pub struct StudioState {
    pub auth: Option<StudioAuth>,
    pub memory: Arc<Store>,
    pub telemetry: Arc<TelemetrySink>,
}
