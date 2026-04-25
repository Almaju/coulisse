//! Server-side studio UI for Coulisse: a read-only browser onto the
//! conversations, memories, scores, and telemetry the proxy has captured.
//!
//! The crate is decoupled from `proxy` — it depends on `memory`,
//! `telemetry`, and `config` (for the auth schema). The cli composes its
//! [`router`] with the proxy router into the same axum service.

mod auth;
mod config;
mod router;
mod state;
mod templates;
mod views;

pub use auth::{OidcBuildError, OidcRuntime, StudioAuth, StudioCredentials};
pub use config::{StudioBasicConfig, StudioConfig, StudioOidcConfig};
pub use router::router;
pub use state::StudioState;
