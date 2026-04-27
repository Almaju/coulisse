use axum::Router;
use axum::body::Body;
use axum::error_handling::HandleErrorLayer;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum_oidc::error::MiddlewareError;
use axum_oidc::{EmptyAdditionalClaims, OidcAuthLayer, OidcClient, OidcLoginLayer};
use base64::Engine;
use http::Uri;
use thiserror::Error;
use time::Duration;
use tower::ServiceBuilder;
use tower_sessions::cookie::SameSite;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};

use crate::config::{Config, OidcConfig, ScopeConfig};

/// Runtime auth state, built once from YAML at startup. Each scope (`proxy`
/// for `/v1/*`, `admin` for `/admin/*`) holds its own optional [`Scheme`].
#[derive(Clone)]
pub struct Auth {
    admin: Option<Scheme>,
    proxy: Option<Scheme>,
}

#[derive(Clone)]
enum Scheme {
    Basic(Credentials),
    Oidc(Box<OidcRuntime>),
}

#[derive(Clone, Debug)]
struct Credentials {
    password: String,
    username: String,
}

impl Credentials {
    fn new(username: String, password: String) -> Self {
        Self { password, username }
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

#[derive(Clone)]
struct OidcRuntime {
    client: OidcClient<EmptyAdditionalClaims>,
}

impl OidcRuntime {
    /// Contact the issuer's `/.well-known/openid-configuration` endpoint
    /// and assemble a ready-to-use client. `openid` is added to the scope
    /// list unconditionally — it's required by the protocol and omitting
    /// it from YAML shouldn't silently break login.
    async fn discover(config: &OidcConfig) -> Result<Self, BuildError> {
        let base = Uri::try_from(&config.redirect_url).map_err(|source| BuildError::BaseUrl {
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
        .map_err(BuildError::Discovery)?;
        Ok(Self { client })
    }
}

impl Auth {
    /// Build runtime state from YAML. Validation is the caller's job — call
    /// `Config::validate()` first; OIDC discovery is the only network step
    /// performed here, and any failure surfaces as a fatal startup error.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn from_config(config: Config) -> Result<Self, BuildError> {
        let admin = match config.admin {
            Some(scope) => Some(Scheme::from_config(scope).await?),
            None => None,
        };
        let proxy = match config.proxy {
            Some(scope) => Some(Scheme::from_config(scope).await?),
            None => None,
        };
        Ok(Self { admin, proxy })
    }

    /// Wrap the `/v1/*` proxy router in the configured proxy-scope auth
    /// layers, or return it unchanged when no proxy auth is configured.
    pub fn wrap_proxy(&self, router: Router) -> Router {
        match &self.proxy {
            Some(scheme) => apply(router, scheme),
            None => router,
        }
    }

    /// Wrap the `/admin/*` router in the configured admin-scope auth
    /// layers, or return it unchanged when no admin auth is configured.
    pub fn wrap_admin(&self, router: Router) -> Router {
        match &self.admin {
            Some(scheme) => apply(router, scheme),
            None => router,
        }
    }

    /// One-line description of the proxy-scope auth posture, for the
    /// startup banner.
    #[must_use]
    pub fn proxy_summary(&self) -> &'static str {
        Self::summary(self.proxy.as_ref())
    }

    /// One-line description of the admin-scope auth posture, for the
    /// startup banner.
    #[must_use]
    pub fn admin_summary(&self) -> &'static str {
        Self::summary(self.admin.as_ref())
    }

    fn summary(scheme: Option<&Scheme>) -> &'static str {
        match scheme {
            None => "unauthenticated",
            Some(Scheme::Basic(_)) => "basic auth enabled",
            Some(Scheme::Oidc(_)) => "OIDC login enabled",
        }
    }
}

impl Scheme {
    async fn from_config(scope: ScopeConfig) -> Result<Self, BuildError> {
        if let Some(basic) = scope.basic {
            return Ok(Scheme::Basic(Credentials::new(
                basic.username,
                basic.password,
            )));
        }
        if let Some(oidc) = scope.oidc {
            let runtime = OidcRuntime::discover(&oidc).await?;
            return Ok(Scheme::Oidc(Box::new(runtime)));
        }
        // Validation is the caller's contract; reaching here means the
        // caller skipped `Config::validate`.
        Err(BuildError::ScopeWithoutAuth)
    }
}

fn apply(router: Router, scheme: &Scheme) -> Router {
    match scheme {
        Scheme::Basic(creds) => {
            let creds = creds.clone();
            router.layer(from_fn(move |req: Request, next: Next| {
                let creds = creds.clone();
                async move { basic_check(creds, req, next).await }
            }))
        }
        Scheme::Oidc(runtime) => {
            // Session → auth (reads session, sets extensions) → login
            // (forces redirect when no valid ID token). `.layer()` calls
            // are applied outermost-last; session must wrap everything so
            // the OIDC layers find it in request extensions.
            // `HandleErrorLayer` converts the OIDC middlewares'
            // `MiddlewareError` into axum-compatible `Infallible`
            // responses.
            let session = SessionManagerLayer::new(MemoryStore::default())
                .with_same_site(SameSite::Lax)
                .with_expiry(Expiry::OnInactivity(Duration::hours(8)));
            let oidc_login = ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_oidc_error))
                .layer(OidcLoginLayer::<EmptyAdditionalClaims>::new());
            let oidc_auth = ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_oidc_error))
                .layer(OidcAuthLayer::<EmptyAdditionalClaims>::new(
                    runtime.client.clone(),
                ));
            router.layer(oidc_login).layer(oidc_auth).layer(session)
        }
    }
}

async fn basic_check(creds: Credentials, request: Request, next: Next) -> Response {
    let ok = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| creds.verify_header(h));
    if ok {
        next.run(request).await
    } else {
        unauthorized()
    }
}

async fn handle_oidc_error(err: MiddlewareError) -> Response {
    err.into_response()
}

/// 401 with the `WWW-Authenticate: Basic` challenge that tells browsers
/// to pop the login dialog. Realm is fixed so bookmarked pages prompt
/// once per origin, not per path.
fn unauthorized() -> Response {
    let mut response = Response::new(Body::from("authentication required"));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        r#"Basic realm="Coulisse", charset="UTF-8""#
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

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("oidc redirect_url is not a valid URI ({value:?}): {source}")]
    BaseUrl {
        source: http::uri::InvalidUri,
        value: String,
    },
    #[error("failed to discover OIDC issuer: {0}")]
    Discovery(axum_oidc::error::Error),
    #[error("auth scope declared without basic or oidc — call Config::validate first")]
    ScopeWithoutAuth,
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
        let creds = Credentials::new("admin".into(), "s3cret".into());
        assert!(creds.verify_header(&header_for("admin", "s3cret")));
    }

    #[test]
    fn wrong_password_rejected() {
        let creds = Credentials::new("admin".into(), "s3cret".into());
        assert!(!creds.verify_header(&header_for("admin", "wrong")));
    }

    #[test]
    fn wrong_username_rejected() {
        let creds = Credentials::new("admin".into(), "s3cret".into());
        assert!(!creds.verify_header(&header_for("root", "s3cret")));
    }

    #[test]
    fn non_basic_scheme_rejected() {
        let creds = Credentials::new("admin".into(), "s3cret".into());
        assert!(!creds.verify_header("Bearer abc"));
    }

    #[test]
    fn malformed_base64_rejected() {
        let creds = Credentials::new("admin".into(), "s3cret".into());
        assert!(!creds.verify_header("Basic !!!not-base64!!!"));
    }

    #[test]
    fn missing_colon_rejected() {
        let creds = Credentials::new("admin".into(), "s3cret".into());
        let encoded = base64::engine::general_purpose::STANDARD.encode("no-colon-here");
        assert!(!creds.verify_header(&format!("Basic {encoded}")));
    }

    #[test]
    fn lowercase_basic_scheme_accepted() {
        // RFC 7235 makes the scheme case-insensitive; some clients lowercase it.
        let creds = Credentials::new("admin".into(), "s3cret".into());
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:s3cret");
        assert!(creds.verify_header(&format!("basic {encoded}")));
    }
}
