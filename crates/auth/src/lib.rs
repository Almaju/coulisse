//! Authentication for Coulisse.
//!
//! Two scopes are configured independently in YAML:
//!
//! - `proxy` guards the OpenAI-compatible `/v1/*` surface that SDK clients
//!   call.
//! - `admin` guards the `/admin/*` surface (the studio UI and any future
//!   admin endpoints).
//!
//! Each scope can be unauthenticated, HTTP Basic, OIDC, or — on the proxy
//! scope only — self-issued API tokens. Cli applies [`Auth::wrap_proxy`] /
//! [`Auth::wrap_admin`] to the respective routers at startup. The token
//! scheme also owns a studio admin page ([`admin::router`]) for minting,
//! monitoring spend on, and revoking tokens.

pub mod admin;
mod auth;
mod config;
mod token;

pub use auth::{Auth, AuthenticatedPrincipal, AuthenticatedToken, BuildError};
pub use config::{
    BasicConfig, Config, ConfigError, IdentityMode, OidcConfig, ScopeConfig, TokensConfig,
};
pub use token::{
    Budget, BudgetError, BudgetParseError, MintedToken, StoreError, TokenId, TokenRecord,
    TokenStore, VerifiedToken, micro_to_usd,
};
