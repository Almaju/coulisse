//! Authentication for Coulisse.
//!
//! Two scopes are configured independently in YAML:
//!
//! - `proxy` guards the OpenAI-compatible `/v1/*` surface that SDK clients
//!   call.
//! - `admin` guards the `/admin/*` surface (the studio UI and any future
//!   admin endpoints).
//!
//! Each scope can be unauthenticated, HTTP Basic, or OIDC. Cli applies
//! [`Auth::wrap_proxy`] / [`Auth::wrap_admin`] to the respective routers
//! at startup.

mod auth;
mod config;

pub use auth::{Auth, BuildError};
pub use config::{BasicConfig, Config, ConfigError, OidcConfig, ScopeConfig};
