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

use crate::config::{Config, McpAdminConfig, OidcConfig, ScopeConfig};

/// Runtime auth state, built once from YAML at startup. Each scope (`proxy`
/// for `/v1/*`, `admin` for `/admin/*`) holds its own optional [`Scheme`].
/// The `mcp_admin` field holds a bearer token for the MCP admin endpoint.
#[derive(Clone)]
pub struct Auth {
    admin: Option<Scheme>,
    mcp_admin: Option<McpAdminRuntime>,
    proxy: Option<Scheme>,
}

#[derive(Clone)]
struct McpAdminRuntime {
    token: String,
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
        // SAFETY: bitwise `&` (not `&&`) keeps both branches constant-time —
        // short-circuiting would leak via timing which credential failed.
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
            None => None,
            Some(scope) => Some(Scheme::from_config(scope).await?),
        };
        let mcp_admin = config.mcp_admin.map(|c| McpAdminRuntime::from_config(&c));
        let proxy = match config.proxy {
            None => None,
            Some(scope) => Some(Scheme::from_config(scope).await?),
        };
        Ok(Self {
            admin,
            mcp_admin,
            proxy,
        })
    }

    /// Check a bearer token against the `mcp_admin` scope. Returns `true`
    /// only when `mcp_admin` auth is configured and the token matches.
    /// An unconfigured `mcp_admin` always returns `false` — the caller
    /// should treat unconfigured routes as absent, not open.
    #[must_use]
    pub fn check_mcp_admin_bearer(&self, header_value: &str) -> bool {
        let Some(runtime) = &self.mcp_admin else {
            return false;
        };
        let Some(token) = header_value
            .strip_prefix("Bearer ")
            .or_else(|| header_value.strip_prefix("bearer "))
        else {
            return false;
        };
        constant_time_eq(token.trim().as_bytes(), runtime.token.as_bytes())
    }

    /// Whether the MCP admin endpoint is configured (i.e. has a token).
    #[must_use]
    pub fn mcp_admin_enabled(&self) -> bool {
        self.mcp_admin.is_some()
    }

    fn summary(scheme: Option<&Scheme>) -> &'static str {
        match scheme {
            None => "unauthenticated",
            Some(Scheme::Basic(_)) => "basic auth enabled",
            Some(Scheme::Oidc(_)) => "OIDC login enabled",
        }
    }

    /// One-line description of the admin-scope auth posture, for the
    /// startup banner.
    #[must_use]
    pub fn admin_summary(&self) -> &'static str {
        Self::summary(self.admin.as_ref())
    }

    /// One-line description of the proxy-scope auth posture, for the
    /// startup banner.
    #[must_use]
    pub fn proxy_summary(&self) -> &'static str {
        Self::summary(self.proxy.as_ref())
    }

    /// Wrap the `/admin/*` router in the configured admin-scope auth
    /// layers, or return it unchanged when no admin auth is configured.
    pub fn wrap_admin(&self, router: Router) -> Router {
        match &self.admin {
            None => router,
            Some(scheme) => apply(router, scheme),
        }
    }

    /// Wrap the `/v1/*` proxy router in the configured proxy-scope auth
    /// layers, or return it unchanged when no proxy auth is configured.
    pub fn wrap_proxy(&self, router: Router) -> Router {
        match &self.proxy {
            None => router,
            Some(scheme) => apply(router, scheme),
        }
    }
}

impl McpAdminRuntime {
    fn from_config(config: &McpAdminConfig) -> Self {
        Self {
            token: config.token.clone(),
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
        // WHY: validation is the caller's contract — reaching here means the
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
            // WHY: layer order is session → auth → login. `.layer()` calls
            // are applied outermost-last, so session must wrap everything for
            // the OIDC layers to find it in request extensions.
            // `HandleErrorLayer` converts the OIDC middlewares'
            // `MiddlewareError` into axum-compatible `Infallible` responses.
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

    fn bearer_header(token: &str) -> String {
        format!("Bearer {token}")
    }

    fn mcp_auth_with(token: &str) -> Auth {
        Auth {
            admin: None,
            mcp_admin: Some(McpAdminRuntime {
                token: token.to_string(),
            }),
            proxy: None,
        }
    }

    #[test]
    fn mcp_admin_valid_token_accepted() {
        let auth = mcp_auth_with("secret-token");
        assert!(auth.check_mcp_admin_bearer(&bearer_header("secret-token")));
    }

    #[test]
    fn mcp_admin_wrong_token_rejected() {
        let auth = mcp_auth_with("secret-token");
        assert!(!auth.check_mcp_admin_bearer(&bearer_header("wrong")));
    }

    #[test]
    fn mcp_admin_no_config_always_rejected() {
        let auth = Auth {
            admin: None,
            mcp_admin: None,
            proxy: None,
        };
        assert!(!auth.check_mcp_admin_bearer(&bearer_header("secret-token")));
    }

    #[test]
    fn mcp_admin_basic_scheme_rejected() {
        let auth = mcp_auth_with("secret-token");
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:secret-token");
        assert!(!auth.check_mcp_admin_bearer(&format!("Basic {encoded}")));
    }

    #[test]
    fn mcp_admin_lowercase_bearer_accepted() {
        let auth = mcp_auth_with("tok");
        assert!(auth.check_mcp_admin_bearer("bearer tok"));
    }

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
        // NOTE: RFC 7235 makes the scheme case-insensitive; some clients lowercase it.
        let creds = Credentials::new("admin".into(), "s3cret".into());
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:s3cret");
        assert!(creds.verify_header(&format!("basic {encoded}")));
    }
}
