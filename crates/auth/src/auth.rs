use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::error_handling::HandleErrorLayer;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum_oidc::error::MiddlewareError;
use axum_oidc::{EmptyAdditionalClaims, OidcAuthLayer, OidcClaims, OidcClient, OidcLoginLayer};
use base64::Engine;
use http::Uri;
use thiserror::Error;
use time::Duration;
use tower::ServiceBuilder;
use tower_sessions::cookie::SameSite;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};

use crate::config::{Config, OidcConfig, ScopeConfig};
use crate::token::{TokenId, TokenStore};

/// Runtime auth state, built once from YAML at startup. Each scope (`proxy`
/// for `/v1/*`, `admin` for `/admin/*`) holds its own optional [`Scheme`].
#[derive(Clone)]
pub struct Auth {
    admin: Option<Scheme>,
    proxy: Option<Scheme>,
}

/// The identity established by a successful authentication, inserted into
/// request extensions so the request-flow handler can bind the user
/// identity to the credential instead of trusting the request body. Carries
/// the Basic username or the OIDC `sub` claim. Absent when the scope is
/// unauthenticated.
#[derive(Clone, Debug)]
pub struct AuthenticatedPrincipal(pub String);

/// The id of the API token a request authenticated with, inserted into
/// request extensions alongside [`AuthenticatedPrincipal`] when the proxy
/// scope uses the `tokens` scheme. The request-flow handler reads it to
/// enforce the token's budget and attribute spend. Absent for Basic/OIDC
/// scopes and for unauthenticated requests.
#[derive(Clone, Copy, Debug)]
pub struct AuthenticatedToken(pub TokenId);

#[derive(Clone)]
enum Scheme {
    ApiKey(Arc<TokenStore>),
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
    pub async fn from_config(
        config: Config,
        token_store: Option<Arc<TokenStore>>,
    ) -> Result<Self, BuildError> {
        let admin = match config.admin {
            None => None,
            Some(scope) => Some(Scheme::from_config(scope, token_store.clone()).await?),
        };
        let proxy = match config.proxy {
            None => None,
            Some(scope) => Some(Scheme::from_config(scope, token_store).await?),
        };
        Ok(Self { admin, proxy })
    }

    fn summary(scheme: Option<&Scheme>) -> &'static str {
        match scheme {
            None => "unauthenticated",
            Some(Scheme::ApiKey(_)) => "API tokens enabled",
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

impl Scheme {
    async fn from_config(
        scope: ScopeConfig,
        token_store: Option<Arc<TokenStore>>,
    ) -> Result<Self, BuildError> {
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
        if scope.tokens.is_some() {
            // WHY: cli only opens the token store when a scope declares
            // `tokens`, so a missing store here means the wiring skipped a
            // step — fail startup rather than serve an unguarded proxy.
            let store = token_store.ok_or(BuildError::TokenStoreUnavailable)?;
            return Ok(Scheme::ApiKey(store));
        }
        // WHY: validation is the caller's contract — reaching here means the
        // caller skipped `Config::validate`.
        Err(BuildError::ScopeWithoutAuth)
    }
}

fn apply(router: Router, scheme: &Scheme) -> Router {
    match scheme {
        Scheme::ApiKey(store) => {
            let store = Arc::clone(store);
            router.layer(from_fn(move |req: Request, next: Next| {
                let store = Arc::clone(&store);
                async move { api_key_check(store, req, next).await }
            }))
        }
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
            // WHY: `oidc_principal` is the innermost layer (first `.layer()`
            // call), so it runs after `oidc_auth` has populated the claims —
            // it reads the `sub` and hands the handler a credential-bound
            // principal.
            router
                .layer(from_fn(oidc_principal))
                .layer(oidc_login)
                .layer(oidc_auth)
                .layer(session)
        }
    }
}

/// Verify the `Authorization: Bearer sk-coulisse-…` header against the token
/// store. On a hit, lift the bound principal and token id into request
/// extensions so the handler binds identity to the credential and enforces
/// the token's budget. A miss is 401; a store error is 500.
async fn api_key_check(store: Arc<TokenStore>, mut request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_secret);
    let Some(secret) = presented else {
        return unauthorized(r#"Bearer realm="Coulisse""#);
    };
    match store.verify(secret).await {
        Ok(Some(verified)) => {
            request
                .extensions_mut()
                .insert(AuthenticatedPrincipal(verified.principal));
            request
                .extensions_mut()
                .insert(AuthenticatedToken(verified.id));
            next.run(request).await
        }
        Ok(None) => unauthorized(r#"Bearer realm="Coulisse", error="invalid_token""#),
        Err(_) => {
            let mut response = Response::new(Body::from("token verification failed"));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        }
    }
}

/// Extract the credential from a `Bearer <secret>` header value,
/// case-insensitively on the scheme (RFC 7235). `None` for any other scheme.
fn bearer_secret(header_value: &str) -> Option<&str> {
    header_value
        .strip_prefix("Bearer ")
        .or_else(|| header_value.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

async fn basic_check(creds: Credentials, mut request: Request, next: Next) -> Response {
    let ok = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| creds.verify_header(h));
    if ok {
        request
            .extensions_mut()
            .insert(AuthenticatedPrincipal(creds.username.clone()));
        next.run(request).await
    } else {
        unauthorized(r#"Basic realm="Coulisse", charset="UTF-8""#)
    }
}

/// Lift the OIDC subject the auth layer cached into request extensions onto
/// an [`AuthenticatedPrincipal`], so the principal is uniform across auth
/// schemes by the time the handler runs.
async fn oidc_principal(mut request: Request, next: Next) -> Response {
    if let Some(claims) = request
        .extensions()
        .get::<OidcClaims<EmptyAdditionalClaims>>()
    {
        let subject = claims.subject().to_string();
        request
            .extensions_mut()
            .insert(AuthenticatedPrincipal(subject));
    }
    next.run(request).await
}

async fn handle_oidc_error(err: MiddlewareError) -> Response {
    err.into_response()
}

/// 401 carrying the given `WWW-Authenticate` challenge. For Basic this tells
/// browsers to pop the login dialog; for Bearer it names the scheme SDK
/// clients expect. Realm is fixed so bookmarked pages prompt once per
/// origin, not per path.
fn unauthorized(challenge: &'static str) -> Response {
    let mut response = Response::new(Body::from("authentication required"));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        challenge.parse().expect("static header value"),
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
    #[error("auth scope declared without basic, oidc, or tokens — call Config::validate first")]
    ScopeWithoutAuth,
    #[error("auth scope declares `tokens` but no token store was provided to Auth::from_config")]
    TokenStoreUnavailable,
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
        // NOTE: RFC 7235 makes the scheme case-insensitive; some clients lowercase it.
        let creds = Credentials::new("admin".into(), "s3cret".into());
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:s3cret");
        assert!(creds.verify_header(&format!("basic {encoded}")));
    }

    use crate::config::TokensConfig;
    use crate::token::{Budget, TokenStore};
    use axum::Extension;
    use axum::routing::get;
    use tower::ServiceExt;

    async fn token_app(principal: &str) -> (Router, String, std::sync::Arc<TokenStore>) {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = std::sync::Arc::new(TokenStore::open(pool).await.unwrap());
        let secret = store
            .mint("test", principal, Budget::Unlimited)
            .await
            .unwrap()
            .secret;
        let config = Config {
            admin: None,
            mcp_admin: None,
            mcp_consumer_secret: None,
            proxy: Some(ScopeConfig {
                basic: None,
                identity: crate::IdentityMode::default(),
                oidc: None,
                tokens: Some(TokensConfig {}),
            }),
        };
        let auth = Auth::from_config(config, Some(std::sync::Arc::clone(&store)))
            .await
            .unwrap();
        // Handler echoes the bound principal so the test sees that the
        // middleware lifted it into extensions.
        let router = Router::new().route(
            "/v1/models",
            get(
                |principal: Option<Extension<AuthenticatedPrincipal>>| async move {
                    principal.map(|Extension(p)| p.0).unwrap_or_default()
                },
            ),
        );
        (auth.wrap_proxy(router), secret, store)
    }

    fn get_models(bearer: Option<&str>) -> Request {
        let mut builder = Request::builder().uri("/v1/models");
        if let Some(value) = bearer {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn api_key_rejects_missing_and_unknown_secrets() {
        let (app, _secret, _store) = token_app("alice").await;
        let missing = app.clone().oneshot(get_models(None)).await.unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        let unknown = app
            .oneshot(get_models(Some("Bearer sk-coulisse-nope")))
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_key_accepts_valid_and_binds_principal() {
        let (app, secret, _store) = token_app("alice").await;
        let resp = app
            .oneshot(get_models(Some(&format!("Bearer {secret}"))))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"alice");
    }

    #[tokio::test]
    async fn api_key_rejects_revoked_token() {
        let (app, secret, store) = token_app("alice").await;
        let id = store.list().await.unwrap()[0].id;
        assert!(store.revoke(id).await.unwrap());
        let resp = app
            .oneshot(get_models(Some(&format!("Bearer {secret}"))))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
