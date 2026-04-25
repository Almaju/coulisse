//! Authentication for the studio UI.
//!
//! Two modes, mutually exclusive at the YAML layer:
//!
//! - **Basic auth** (`studio.basic`): static username/password checked in a
//!   middleware. Browsers prompt via the native login dialog on
//!   `WWW-Authenticate: Basic` and cache credentials per origin.
//! - **OIDC** (`studio.oidc`): full authorization-code-with-PKCE login flow
//!   against any OIDC-compliant IdP (Authentik, Keycloak, Google, etc.).
//!   Implemented via `axum-oidc` + `tower-sessions`, layered onto the
//!   studio routes only so the OpenAI-compatible `/v1/*` routes stay
//!   cookie-free for SDK clients.
//!
//! Access control (who may log in) is intentionally not configured here:
//! for Basic, the credentials themselves are the gate; for OIDC, the IdP's
//! application bindings decide.

use std::sync::Arc;

use crate::StudioOidcConfig;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_oidc::{EmptyAdditionalClaims, OidcClient};
use base64::Engine;
use http::Uri;
use thiserror::Error;

use crate::state::StudioState;

/// Runtime studio-auth state, built once from YAML at startup. The OIDC
/// variant is boxed because its internal openid-connect client is ~1KB
/// while the Basic variant is two short strings.
#[derive(Clone)]
pub enum StudioAuth {
    Basic(StudioCredentials),
    Oidc(Box<OidcRuntime>),
}

/// Parsed Basic-auth credentials. Held in `StudioState`; never mutated
/// after construction.
#[derive(Clone, Debug)]
pub struct StudioCredentials {
    password: String,
    username: String,
}

impl StudioCredentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
            username: username.into(),
        }
    }

    /// Check a raw `Authorization` header value. Returns `true` only if the
    /// scheme is `Basic` and the base64-decoded `user:pass` pair matches.
    fn verify_header(&self, header_value: &str) -> bool {
        let Some(encoded) = header_value
            .strip_prefix("Basic ")
            .or_else(|| header_value.strip_prefix("basic "))
        else {
            return false;
        };
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return false;
        };
        let Ok(pair) = std::str::from_utf8(&decoded) else {
            return false;
        };
        let Some((user, pass)) = pair.split_once(':') else {
            return false;
        };
        // Bitwise `&` (not `&&`) so both comparisons always run.
        constant_time_eq(user.as_bytes(), self.username.as_bytes())
            & constant_time_eq(pass.as_bytes(), self.password.as_bytes())
    }
}

/// Pre-discovered OIDC client, cloned into the auth layer at router
/// construction. Discovery runs once at startup so we fail fast if the
/// issuer is misconfigured.
#[derive(Clone)]
pub struct OidcRuntime {
    pub client: OidcClient<EmptyAdditionalClaims>,
}

impl OidcRuntime {
    /// Contact the issuer's `/.well-known/openid-configuration` endpoint
    /// and assemble a ready-to-use client. `openid` is added to the scope
    /// list unconditionally — it's required by the protocol and omitting
    /// it from YAML shouldn't silently break login.
    pub async fn discover(config: &StudioOidcConfig) -> Result<Self, OidcBuildError> {
        let base =
            Uri::try_from(&config.redirect_url).map_err(|source| OidcBuildError::BaseUrl {
                source,
                value: config.redirect_url.clone(),
            })?;
        let mut scopes = vec!["openid".to_string()];
        scopes.extend(config.scopes.iter().cloned());
        scopes.sort();
        scopes.dedup();
        let client = OidcClient::<EmptyAdditionalClaims>::discover_new(
            base,
            config.issuer_url.clone(),
            config.client_id.clone(),
            config.client_secret.clone(),
            scopes,
        )
        .await
        .map_err(OidcBuildError::Discovery)?;
        Ok(Self { client })
    }
}

#[derive(Debug, Error)]
pub enum OidcBuildError {
    #[error("studio.oidc.redirect_url is not a valid URI ({value:?}): {source}")]
    BaseUrl {
        source: http::uri::InvalidUri,
        value: String,
    },
    #[error("failed to discover OIDC issuer: {0}")]
    Discovery(axum_oidc::error::Error),
}

/// Tower middleware for the Basic-auth branch. When `state.auth` is
/// `Some(StudioAuth::Basic(_))`, rejects requests that don't carry a
/// matching `Authorization: Basic` credential. When `None` or `Oidc`,
/// passes through — the OIDC layers (if any) handle those modes on
/// their own.
pub(crate) async fn require_basic_auth(
    State(state): State<Arc<StudioState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(StudioAuth::Basic(creds)) = state.auth.as_ref() else {
        return next.run(request).await;
    };
    let ok = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|h| creds.verify_header(h))
        .unwrap_or(false);
    if ok {
        next.run(request).await
    } else {
        unauthorized()
    }
}

/// 401 with the `WWW-Authenticate: Basic` challenge that tells browsers
/// to pop the login dialog. Realm is fixed so bookmarked studio pages
/// prompt once per origin, not per path.
fn unauthorized() -> Response {
    let mut response = Response::new(Body::from("studio authentication required"));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        r#"Basic realm="Coulisse studio", charset="UTF-8""#
            .parse()
            .expect("static header value"),
    );
    response.into_response()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_for(user: &str, pass: &str) -> String {
        let pair = format!("{user}:{pass}");
        let encoded = base64::engine::general_purpose::STANDARD.encode(pair);
        format!("Basic {encoded}")
    }

    #[test]
    fn matching_credentials_accepted() {
        let creds = StudioCredentials::new("admin", "s3cret");
        assert!(creds.verify_header(&header_for("admin", "s3cret")));
    }

    #[test]
    fn wrong_password_rejected() {
        let creds = StudioCredentials::new("admin", "s3cret");
        assert!(!creds.verify_header(&header_for("admin", "wrong")));
    }

    #[test]
    fn wrong_username_rejected() {
        let creds = StudioCredentials::new("admin", "s3cret");
        assert!(!creds.verify_header(&header_for("root", "s3cret")));
    }

    #[test]
    fn non_basic_scheme_rejected() {
        let creds = StudioCredentials::new("admin", "s3cret");
        assert!(!creds.verify_header("Bearer abc"));
    }

    #[test]
    fn malformed_base64_rejected() {
        let creds = StudioCredentials::new("admin", "s3cret");
        assert!(!creds.verify_header("Basic !!!not-base64!!!"));
    }

    #[test]
    fn missing_colon_rejected() {
        let creds = StudioCredentials::new("admin", "s3cret");
        let encoded = base64::engine::general_purpose::STANDARD.encode("no-colon-here");
        assert!(!creds.verify_header(&format!("Basic {encoded}")));
    }

    #[test]
    fn lowercase_basic_scheme_accepted() {
        // RFC 7235 makes the scheme case-insensitive; some clients lowercase it.
        let creds = StudioCredentials::new("admin", "s3cret");
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:s3cret");
        assert!(creds.verify_header(&format!("basic {encoded}")));
    }
}
