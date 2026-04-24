//! Authentication for the admin UI and its JSON API.
//!
//! Two modes, mutually exclusive at the YAML layer:
//!
//! - **Basic auth** (`admin.basic`): static username/password checked in a
//!   middleware. Browsers prompt via the native login dialog on
//!   `WWW-Authenticate: Basic`, cache credentials per origin, and replay
//!   them on subsequent XHRs — so the Leptos SPA needs no awareness of auth.
//! - **OIDC** (`admin.oidc`): full authorization-code-with-PKCE login flow
//!   against any OIDC-compliant IdP (Authentik, Keycloak, Google, etc.).
//!   Implemented via `axum-oidc` + `tower-sessions`, layered onto the
//!   `/admin/*` subtree only so the OpenAI-compatible `/v1/*` routes
//!   stay cookie-free for SDK clients.
//!
//! Access control (who may log in) is intentionally not configured here:
//! for Basic, the credentials themselves are the gate; for OIDC, the IdP's
//! application bindings decide. Coulisse treats "successfully authenticated"
//! as "authorized admin" to keep the surface area small.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_oidc::{EmptyAdditionalClaims, OidcClient};
use base64::Engine;
use http::Uri;
use prompter::{AdminOidcConfig, Prompter};
use thiserror::Error;

use crate::AppState;

/// Runtime admin-auth state, built once from YAML at startup. The OIDC
/// variant is boxed because its internal openid-connect client is ~1KB
/// while the Basic variant is two short strings — keeping the enum small
/// avoids stack bloat on every handler that carries `AppState`.
#[derive(Clone)]
pub enum AdminAuth {
    Basic(AdminCredentials),
    Oidc(Box<OidcRuntime>),
}

/// Parsed Basic-auth credentials. Held in `AppState`; never mutated after
/// construction.
#[derive(Clone, Debug)]
pub struct AdminCredentials {
    password: String,
    username: String,
}

impl AdminCredentials {
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
    /// list unconditionally — it's required by the protocol and omitting it
    /// from YAML shouldn't silently break login.
    pub async fn discover(config: &AdminOidcConfig) -> Result<Self, OidcBuildError> {
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
    #[error("admin.oidc.redirect_url is not a valid URI ({value:?}): {source}")]
    BaseUrl {
        source: http::uri::InvalidUri,
        value: String,
    },
    #[error("failed to discover OIDC issuer: {0}")]
    Discovery(axum_oidc::error::Error),
}

/// Tower middleware for the Basic-auth branch. When `state.admin_auth` is
/// `Some(AdminAuth::Basic(_))`, rejects requests that don't carry matching
/// `Authorization: Basic` credentials. When `None` or `Oidc`, passes
/// through — the OIDC layers (if any) handle those modes on their own.
pub async fn require_basic_auth<P: Prompter>(
    State(state): State<Arc<AppState<P>>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(AdminAuth::Basic(creds)) = state.admin_auth.as_ref() else {
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

/// 401 with the `WWW-Authenticate: Basic` challenge that tells browsers to
/// pop the login dialog. Realm is fixed so bookmarked admin pages prompt
/// once per origin, not per path.
fn unauthorized() -> Response {
    let mut response = Response::new(Body::from("admin authentication required"));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        r#"Basic realm="Coulisse admin", charset="UTF-8""#
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
        let creds = AdminCredentials::new("admin", "s3cret");
        assert!(creds.verify_header(&header_for("admin", "s3cret")));
    }

    #[test]
    fn wrong_password_rejected() {
        let creds = AdminCredentials::new("admin", "s3cret");
        assert!(!creds.verify_header(&header_for("admin", "wrong")));
    }

    #[test]
    fn wrong_username_rejected() {
        let creds = AdminCredentials::new("admin", "s3cret");
        assert!(!creds.verify_header(&header_for("root", "s3cret")));
    }

    #[test]
    fn non_basic_scheme_rejected() {
        let creds = AdminCredentials::new("admin", "s3cret");
        assert!(!creds.verify_header("Bearer abc"));
    }

    #[test]
    fn malformed_base64_rejected() {
        let creds = AdminCredentials::new("admin", "s3cret");
        assert!(!creds.verify_header("Basic !!!not-base64!!!"));
    }

    #[test]
    fn missing_colon_rejected() {
        let creds = AdminCredentials::new("admin", "s3cret");
        let encoded = base64::engine::general_purpose::STANDARD.encode("no-colon-here");
        assert!(!creds.verify_header(&format!("Basic {encoded}")));
    }

    #[test]
    fn lowercase_basic_scheme_accepted() {
        // Some clients lowercase the scheme; RFC 7235 makes it case-insensitive.
        let creds = AdminCredentials::new("admin", "s3cret");
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:s3cret");
        assert!(creds.verify_header(&format!("basic {encoded}")));
    }
}
