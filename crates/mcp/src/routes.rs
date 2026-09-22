use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use coulisse_core::UserId;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::config::{McpOAuthConfig, McpServerConfig, McpTransport};
use crate::discovery::AuthMetadata;
use crate::error::McpError;
use crate::oauth::{
    ClientId, ClientSecret, CodeChallenge, CodeVerifier, PkcePair, RedirectUri, StateKey,
    StateToken, TokenResponse,
};
use crate::vault::{StoredClient, TokenVault};

/// Origin (scheme + host + port, optional path prefix) at which this
/// Coulisse instance is reachable from a browser. Trailing slashes are
/// dropped at construction so paths can be appended verbatim.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PublicBaseUrl(String);

impl PublicBaseUrl {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into().trim_end_matches('/').to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PublicBaseUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Bearer secret that authenticates admin calls to
/// `POST /mcp/{server}/connect-link` (`auth.mcp_consumer_secret`).
#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ConsumerSecret(String);

impl ConsumerSecret {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ConsumerSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsumerSecret([redacted])")
    }
}

/// Everything `NotConnectedTool` needs to mint a per-user connect URL
/// it can hand the LLM. Cloneable so each session can carry one.
#[derive(Clone, Debug)]
pub struct ConnectLinkSigner {
    key: StateKey,
    public_base_url: PublicBaseUrl,
}

impl ConnectLinkSigner {
    /// Build from the base64-encoded 32-byte `hmac_key` and the origin
    /// users reach Coulisse at.
    ///
    /// # Errors
    ///
    /// Returns `McpError::StateKeyInvalid` if the key is not valid base64
    /// or not exactly 32 bytes after decoding.
    pub fn new(hmac_key_b64: &str, public_base_url: PublicBaseUrl) -> Result<Self, McpError> {
        Ok(Self {
            key: StateKey::from_base64(hmac_key_b64)?,
            public_base_url,
        })
    }

    /// Build the `GET /mcp/{server}/connect?token=...` URL that, when
    /// followed in a browser, validates the token, lazily runs OAuth
    /// discovery + DCR if needed, and redirects to the provider's
    /// authorization endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the state token cannot be encrypted.
    pub fn connect_url(&self, server: &str, user_id: UserId) -> Result<String, McpError> {
        let token = StateToken {
            code_verifier: None,
            server: server.to_string(),
            user_id,
        }
        .encrypt(&self.key)?;
        Ok(format!(
            "{}/mcp/{}/connect?token={}",
            self.public_base_url,
            urlencode(server),
            urlencode(&token),
        ))
    }

    fn redirect_uri_for(&self, server: &str) -> RedirectUri {
        RedirectUri::new(format!(
            "{}/mcp/{}/oauth/callback",
            self.public_base_url,
            urlencode(server),
        ))
    }
}

#[derive(Clone)]
pub struct OAuthRouterState {
    pub configs: HashMap<String, McpServerConfig>,
    pub consumer_secret: Option<ConsumerSecret>,
    pub signer: ConnectLinkSigner,
    pub vault: Arc<TokenVault>,
}

#[derive(Deserialize)]
pub struct ConnectLinkQuery {
    pub user_id: Option<String>,
}

#[derive(Serialize)]
pub struct ConnectLinkResponse {
    pub url: String,
}

#[derive(Deserialize)]
pub struct ConnectQuery {
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub error: Option<String>,
    pub state: Option<String>,
}

impl OAuthRouterState {
    /// OAuth-related routes. Mounted outside proxy/admin auth wrappers at
    /// the application root.
    ///
    /// - `POST /mcp/{server}/connect-link` — admin-facing. Bearer-authed with
    ///   the consumer secret. Returns the authorize URL as JSON for a given
    ///   `user_id`. Used by scripts/admin UIs.
    /// - `GET /mcp/{server}/connect` — user-facing. Auth is the per-user HMAC
    ///   token embedded in the URL (minted by `NotConnectedTool`). 302s to
    ///   the authorize URL; lazily runs discovery + DCR for `discover` mode
    ///   on first hit.
    /// - `GET /mcp/{server}/oauth/callback` — the provider's redirect target.
    ///   Exchanges the code for tokens and stores them in the vault keyed by
    ///   `(server, user_id)`.
    pub fn router(self) -> Router {
        Router::new()
            .route("/mcp/{server}/connect", get(Self::connect))
            .route("/mcp/{server}/connect-link", post(Self::connect_link))
            .route("/mcp/{server}/oauth/callback", get(Self::oauth_callback))
            .with_state(self)
    }

    async fn connect(
        State(state): State<Self>,
        Path(server): Path<String>,
        Query(query): Query<ConnectQuery>,
    ) -> Response {
        let Some(token) = query.token else {
            return (
                StatusCode::BAD_REQUEST,
                Html(error_page("Missing token parameter.")),
            )
                .into_response();
        };

        let token_state = match StateToken::decrypt(&state.signer.key, &token) {
            Ok(s) => s,
            Err(McpError::StateExpired) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Html(error_page(
                        "Connect link has expired. Ask the assistant for a new one.",
                    )),
                )
                    .into_response();
            }
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Html(error_page("Invalid connect link.")),
                )
                    .into_response();
            }
        };

        if token_state.server != server {
            return (
                StatusCode::BAD_REQUEST,
                Html(error_page("Connect link does not match this server.")),
            )
                .into_response();
        }

        let cfg = match state.configs.get(&server) {
            Some(c) if c.oauth.is_some() => c.clone(),
            Some(_) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Html(error_page(&format!(
                        "MCP server '{server}' has no oauth block configured."
                    ))),
                )
                    .into_response();
            }
            None => {
                return (StatusCode::NOT_FOUND, Html(error_page("Unknown server.")))
                    .into_response();
            }
        };

        let resolved = match state.resolve(&server, &cfg).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %server, error = %e, "resolve oauth params failed");
                return (
                    StatusCode::BAD_GATEWAY,
                    Html(error_page(&format!(
                        "Failed to prepare authorization for '{server}'. Check Coulisse logs."
                    ))),
                )
                    .into_response();
            }
        };

        let pkce = PkcePair::generate();
        let new_state = StateToken {
            code_verifier: Some(pkce.verifier),
            server: server.clone(),
            user_id: token_state.user_id,
        }
        .encrypt(&state.signer.key);
        let new_state = match new_state {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(server = %server, error = %e, "failed to seal state token");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html(error_page(
                        "Failed to prepare authorization. Check Coulisse logs.",
                    )),
                )
                    .into_response();
            }
        };
        let url = resolved.authorize_url(&new_state, &pkce.challenge);
        Redirect::to(&url).into_response()
    }

    async fn connect_link(
        State(state): State<Self>,
        Path(server): Path<String>,
        headers: HeaderMap,
        Query(query): Query<ConnectLinkQuery>,
    ) -> Response {
        // The admin connect-link endpoint is opt-in: when no consumer secret
        // is configured, return 503 so unauthenticated callers can't reach
        // it. The per-user `GET /connect` route remains available regardless.
        let Some(consumer_secret) = state
            .consumer_secret
            .as_ref()
            .filter(|s| !s.expose().is_empty())
        else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                concat!(
                    "POST /mcp/{server}/connect-link is disabled because auth.mcp_consumer_secret ",
                    "is not set in coulisse.yaml. The per-user GET /mcp/{server}/connect flow ",
                    "(used by NotConnectedTool) does not require this secret.",
                ),
            )
                .into_response();
        };
        let auth = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let expected = format!("Bearer {}", consumer_secret.expose());
        if !constant_time_eq(auth.as_bytes(), expected.as_bytes()) {
            return StatusCode::UNAUTHORIZED.into_response();
        }

        let Some(user_id) = query.user_id.filter(|s| !s.is_empty()) else {
            return StatusCode::UNPROCESSABLE_ENTITY.into_response();
        };
        let user_id = UserId::from_string(&user_id);

        let cfg = match state.configs.get(&server) {
            Some(c) if c.oauth.is_some() => c.clone(),
            Some(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
            None => return StatusCode::NOT_FOUND.into_response(),
        };

        let resolved = match state.resolve(&server, &cfg).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %server, error = %e, "resolve oauth params failed");
                return (StatusCode::BAD_GATEWAY, e.to_string()).into_response();
            }
        };

        let pkce = PkcePair::generate();
        let state_token = StateToken {
            code_verifier: Some(pkce.verifier),
            server: server.clone(),
            user_id,
        }
        .encrypt(&state.signer.key);
        let state_token = match state_token {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(server = %server, error = %e, "failed to seal state token");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
        let url = resolved.authorize_url(&state_token, &pkce.challenge);
        Json(ConnectLinkResponse { url }).into_response()
    }

    async fn oauth_callback(
        State(state): State<Self>,
        Path(server): Path<String>,
        Query(query): Query<CallbackQuery>,
    ) -> Response {
        if let Some(err) = query.error {
            return (
                StatusCode::BAD_REQUEST,
                Html(error_page(&format!("OAuth provider error: {err}"))),
            )
                .into_response();
        }

        let (Some(code), Some(raw_state)) = (query.code, query.state) else {
            return (
                StatusCode::BAD_REQUEST,
                Html(error_page("Missing code or state parameter")),
            )
                .into_response();
        };

        let token_state = match StateToken::decrypt(&state.signer.key, &raw_state) {
            Ok(s) => s,
            Err(McpError::StateExpired) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Html(error_page(
                        "Authorization link has expired. Please request a new one.",
                    )),
                )
                    .into_response();
            }
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Html(error_page("Invalid state token.")),
                )
                    .into_response();
            }
        };

        if token_state.server != server {
            return (StatusCode::BAD_REQUEST, Html(error_page("Server mismatch"))).into_response();
        }

        let cfg = match state.configs.get(&server) {
            Some(c) if c.oauth.is_some() => c.clone(),
            _ => {
                return (StatusCode::NOT_FOUND, Html(error_page("Unknown server"))).into_response();
            }
        };

        let resolved = match state.resolve(&server, &cfg).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %server, error = %e, "resolve oauth params for callback failed");
                return (
                    StatusCode::BAD_GATEWAY,
                    Html(error_page("Failed to load OAuth parameters.")),
                )
                    .into_response();
            }
        };

        let exchange_result = resolved
            .exchange_code(&code, token_state.code_verifier.as_ref())
            .await;
        let token_response = match exchange_result {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(server = %server, error = %e, "OAuth code exchange failed");
                return (
                    StatusCode::BAD_REQUEST,
                    Html(error_page("Token exchange failed. Please try again.")),
                )
                    .into_response();
            }
        };

        let token = token_response.into_stored_token(None);
        if let Err(e) = state
            .vault
            .upsert_token(&server, token_state.user_id, &token)
            .await
        {
            tracing::error!(server = %server, error = %e, "Failed to store OAuth token");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(error_page("Failed to store token. Please try again.")),
            )
                .into_response();
        }

        Html(success_page(&server)).into_response()
    }

    /// First user to connect to a `discover` server: register Coulisse as
    /// a client (RFC 7591) against the freshly discovered authorization
    /// server and cache the result in the vault for every subsequent user.
    async fn register_client(
        &self,
        server: &str,
        mut metadata: AuthMetadata,
    ) -> Result<StoredClient, McpError> {
        let redirect_uri = self.signer.redirect_uri_for(server);
        let registration = metadata.register_client(server, &redirect_uri).await?;
        // Fold scopes from the RFC 7591 registration response into the
        // cached metadata when the AS itself didn't advertise any.
        // Atlassian's MCP AS reports `scopes_supported: []` in
        // discovery but some providers echo a useful scope set in the
        // registration response.
        if metadata.scopes_supported.is_empty()
            && let Some(dcr_scopes) = registration.scopes.as_deref()
            && !dcr_scopes.is_empty()
        {
            tracing::info!(
                server = %server,
                scopes = ?dcr_scopes,
                "AS metadata omitted scopes_supported; using scopes echoed by DCR response"
            );
            metadata.scopes_supported = dcr_scopes.to_vec();
        }
        let client = StoredClient {
            client_id: registration.client_id,
            client_secret: registration.client_secret,
            metadata,
            redirect_uri,
        };
        self.vault.upsert_client(server, &client).await?;
        Ok(client)
    }

    /// Either pull the cached client registration from the vault or run
    /// discovery + DCR right now and cache the result. Reused across the
    /// `connect-link`, `connect`, and `callback` handlers — they all need the
    /// same parameters but reach this code in different orders.
    async fn resolve(
        &self,
        server: &str,
        cfg: &McpServerConfig,
    ) -> Result<ResolvedOAuth, McpError> {
        let oauth = cfg
            .oauth
            .as_ref()
            .ok_or_else(|| McpError::OAuthNotConfigured {
                server: server.to_string(),
            })?;
        match oauth {
            McpOAuthConfig::Static {
                authorization_url,
                client_id,
                client_secret,
                redirect_uri,
                scopes,
                token_url,
            } => Ok(ResolvedOAuth {
                authorization_endpoint: authorization_url.clone(),
                client_id: ClientId::new(client_id),
                client_secret: Some(ClientSecret::new(client_secret)),
                redirect_uri: RedirectUri::new(redirect_uri),
                resource: None,
                scopes: scopes.clone(),
                token_endpoint: token_url.clone(),
            }),
            McpOAuthConfig::Discover { .. } => {
                // Both the cached-client path and the fresh-discovery path
                // need the MCP endpoint URL for the RFC 8707 `resource`
                // parameter, so resolve transport once up front. Both http
                // and sse transports carry a URL we can drive discovery off.
                let mcp_url = match &cfg.transport {
                    McpTransport::Http { url } | McpTransport::Sse { url } => url,
                    McpTransport::Stdio { .. } => {
                        return Err(McpError::Discovery {
                            source: "oauth: discover requires transport: http or sse (stdio servers have no URL to discover from)".into(),
                            url: format!("<{server}>"),
                        });
                    }
                };
                let client = if let Some(stored) = self.vault.get_client(server).await? {
                    stored
                } else {
                    let metadata = AuthMetadata::discover(mcp_url).await?;
                    self.register_client(server, metadata).await?
                };
                let scopes = oauth.effective_scopes(server, &client.metadata.scopes_supported);
                Ok(ResolvedOAuth {
                    authorization_endpoint: client.metadata.authorization_endpoint,
                    client_id: client.client_id,
                    client_secret: client.client_secret,
                    redirect_uri: client.redirect_uri,
                    resource: Some(mcp_url.clone()),
                    scopes,
                    token_endpoint: client.metadata.token_endpoint,
                })
            }
        }
    }
}

/// Resolved OAuth params for a server, abstracting over `static` (read
/// straight from YAML) and `discover` (read from the vault, populated by
/// discovery + DCR on first user authorisation).
struct ResolvedOAuth {
    authorization_endpoint: String,
    client_id: ClientId,
    client_secret: Option<ClientSecret>,
    redirect_uri: RedirectUri,
    /// RFC 8707 resource indicator — the MCP endpoint URL the token must
    /// be bound to. Sent as `&resource=...` in both the authorize redirect
    /// and the token-exchange `POST`. Some authorization servers (Todoist)
    /// require this; tokens issued without it are scoped to the AS's own
    /// origin and the MCP endpoint rejects them with 401 even though they
    /// "look" valid. `None` for `static` configs since the legacy YAML
    /// shape doesn't carry the resource URL.
    resource: Option<String>,
    scopes: Vec<String>,
    token_endpoint: String,
}

impl ResolvedOAuth {
    fn authorize_url(&self, state_token: &str, code_challenge: &CodeChallenge) -> String {
        let mut url = format!(
            "{}?client_id={}&redirect_uri={}&response_type=code",
            self.authorization_endpoint,
            urlencode(self.client_id.as_str()),
            urlencode(self.redirect_uri.as_str()),
        );
        let scopes = self.scopes.join(" ");
        if !scopes.is_empty() {
            url.push_str("&scope=");
            url.push_str(&urlencode(&scopes));
        }
        if let Some(resource) = self.resource.as_deref() {
            url.push_str("&resource=");
            url.push_str(&urlencode(resource));
        }
        // RFC 7636 / MCP OAuth 2.1: PKCE is mandatory. Without
        // `code_challenge`, some providers (Todoist) still issue an access
        // token but flag it as second-class, and the protected resource at
        // a different origin rejects it with 401.
        url.push_str("&code_challenge=");
        url.push_str(&urlencode(code_challenge.as_str()));
        url.push_str("&code_challenge_method=S256");
        url.push_str("&state=");
        url.push_str(&urlencode(state_token));
        url
    }

    async fn exchange_code(
        &self,
        code: &str,
        code_verifier: Option<&CodeVerifier>,
    ) -> Result<TokenResponse, McpError> {
        let client = reqwest::Client::new();
        let mut params: Vec<(&str, &str)> = vec![
            ("client_id", self.client_id.as_str()),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", self.redirect_uri.as_str()),
        ];
        if let Some(secret) = self.client_secret.as_ref() {
            params.push(("client_secret", secret.expose()));
        }
        // RFC 8707: echo the resource indicator so the AS binds the issued
        // token to the MCP endpoint, not its own origin. Without this,
        // Todoist (and likely others) issues a token that fails 401 when
        // the MCP at a different host sees it.
        if let Some(resource) = self.resource.as_deref() {
            params.push(("resource", resource));
        }
        // RFC 7636: prove possession of the verifier that hashed to the
        // challenge we sent in the authorize request. PKCE-required ASs
        // refuse the exchange without this.
        if let Some(verifier) = code_verifier {
            params.push(("code_verifier", verifier.expose()));
        }
        let response = client
            .post(&self.token_endpoint)
            .header("Accept", "application/json")
            .form(&params)
            .send()
            .await
            .map_err(|source| McpError::TokenExchange {
                server: "unknown".to_string(),
                source,
            })?;
        response
            .json::<TokenResponse>()
            .await
            .map_err(|source| McpError::TokenExchange {
                server: "unknown".to_string(),
                source,
            })
    }
}

fn urlencode(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

fn success_page(server: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="UTF-8"><title>Connected</title>
<style>body{{font-family:system-ui,sans-serif;max-width:600px;margin:4rem auto;padding:0 1rem;text-align:center}}
h1{{color:#16a34a}}</style></head>
<body>
<h1>✓ Connected</h1>
<p>Your <strong>{server}</strong> account is now connected.</p>
<p>You can close this window.</p>
</body></html>"#
    )
}

fn error_page(msg: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="UTF-8"><title>Error</title>
<style>body{{font-family:system-ui,sans-serif;max-width:600px;margin:4rem auto;padding:0 1rem;text-align:center}}
h1{{color:#dc2626}}</style></head>
<body>
<h1>Authorization failed</h1>
<p>{msg}</p>
</body></html>"#
    )
}

// rabot: allow(primitive-soup) symmetric: swapping a and b yields the same result
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
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use base64::Engine as _;
    use coulisse_core::UserId;
    use http_body_util::BodyExt;
    use sqlx::sqlite::SqlitePoolOptions;
    use tower::ServiceExt;

    use super::{ConnectLinkSigner, ConsumerSecret, OAuthRouterState, PublicBaseUrl};
    use crate::config::{McpOAuthConfig, McpServerConfig, McpTransport};
    use crate::oauth::{ClientId, CodeChallenge, RedirectUri, StateToken};
    use crate::vault::{SCHEMA, TokenVault};

    const HMAC_KEY: &[u8] = b"test-hmac-key-32bytes-padding!!!";
    const SECRET: &str = "supersecret";

    fn signer() -> ConnectLinkSigner {
        let key = base64::engine::general_purpose::STANDARD.encode(HMAC_KEY);
        ConnectLinkSigner::new(&key, PublicBaseUrl::new("http://localhost:8421")).unwrap()
    }

    async fn make_state() -> OAuthRouterState {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for stmt in SCHEMA.split(';') {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        let vault_key = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        let vault = Arc::new(TokenVault::new(pool, &vault_key).unwrap());
        let oauth = McpOAuthConfig::Static {
            authorization_url: "https://provider.example.com/auth".into(),
            client_id: "client-id".into(),
            client_secret: "client-secret".into(),
            redirect_uri: "https://coulisse.example.com/mcp/github/oauth/callback".into(),
            scopes: vec!["repo".into()],
            token_url: "https://provider.example.com/token".into(),
        };
        let no_oauth_cfg = McpServerConfig {
            oauth: None,
            transport: McpTransport::Http {
                url: "http://localhost:9999".into(),
            },
        };
        let oauth_cfg = McpServerConfig {
            oauth: Some(oauth),
            transport: McpTransport::Http {
                url: "http://localhost:9999".into(),
            },
        };
        let mut configs = HashMap::new();
        configs.insert("github".into(), oauth_cfg);
        configs.insert("plain".into(), no_oauth_cfg);
        OAuthRouterState {
            configs,
            consumer_secret: Some(ConsumerSecret::new(SECRET)),
            signer: signer(),
            vault,
        }
    }

    async fn call(app: axum::Router, req: Request<Body>) -> axum::response::Response {
        app.oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn connect_link_wrong_secret_returns_401() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/github/connect-link?user_id=u1")
            .header("Authorization", "Bearer wrongsecret")
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn connect_link_unknown_server_returns_404() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/doesnotexist/connect-link?user_id=u1")
            .header("Authorization", format!("Bearer {SECRET}"))
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn connect_link_server_without_oauth_returns_422() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/plain/connect-link?user_id=u1")
            .header("Authorization", format!("Bearer {SECRET}"))
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn connect_link_missing_user_id_returns_422() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/github/connect-link")
            .header("Authorization", format!("Bearer {SECRET}"))
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn connect_link_valid_request_returns_url() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp/github/connect-link?user_id=user-42")
            .header("Authorization", format!("Bearer {SECRET}"))
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let url = v["url"].as_str().unwrap();
        assert!(url.contains("provider.example.com"), "url={url}");
        assert!(url.contains("client_id"), "url={url}");
        assert!(
            url.contains("scope="),
            "url should contain scope, url={url}"
        );
        assert!(url.contains("state="), "url={url}");
        // scope must appear before state per spec
        let scope_pos = url.find("scope=").unwrap();
        let state_pos = url.find("state=").unwrap();
        assert!(
            scope_pos < state_pos,
            "scope should precede state in url={url}"
        );
    }

    #[tokio::test]
    async fn connect_link_user_isolation() {
        // Each user receives a distinct state token; one user's link
        // cannot be replayed to land tokens against another user_id.
        let app = make_state().await.router();

        let mk = |user_id: &str| {
            Request::builder()
                .method("POST")
                .uri(format!("/mcp/github/connect-link?user_id={user_id}"))
                .header("Authorization", format!("Bearer {SECRET}"))
                .body(Body::empty())
                .unwrap()
        };
        let alice = call(app.clone(), mk("alice")).await;
        let bob = call(app, mk("bob")).await;

        let alice_url = serde_json::from_slice::<serde_json::Value>(
            &alice.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap()["url"]
            .as_str()
            .unwrap()
            .to_string();
        let bob_url = serde_json::from_slice::<serde_json::Value>(
            &bob.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap()["url"]
            .as_str()
            .unwrap()
            .to_string();

        let alice_state = alice_url.split("state=").nth(1).unwrap();
        let bob_state = bob_url.split("state=").nth(1).unwrap();
        assert_ne!(alice_state, bob_state);
    }

    #[tokio::test]
    async fn connect_redirects_to_authorize_url() {
        let state = make_state().await;
        let signer = state.signer.clone();
        let app = state.router();

        let connect_url = signer.connect_url("github", UserId::new()).unwrap();
        let path_and_query = connect_url.trim_start_matches("http://localhost:8421");

        let req = Request::builder()
            .method("GET")
            .uri(path_and_query)
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert!(location.contains("provider.example.com"), "loc={location}");
        assert!(location.contains("client_id"), "loc={location}");
        assert!(location.contains("state="), "loc={location}");
    }

    #[tokio::test]
    async fn connect_with_missing_token_returns_400() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("GET")
            .uri("/mcp/github/connect")
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn connect_with_tampered_token_returns_400() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("GET")
            .uri("/mcp/github/connect?token=garbage.sig")
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn connect_token_for_different_server_rejected() {
        let state = make_state().await;
        let signer = state.signer.clone();
        let app = state.router();

        // Mint a token for `plain` but POST it to `/github/connect`.
        let token = StateToken {
            code_verifier: None,
            server: "plain".to_string(),
            user_id: UserId::new(),
        }
        .encrypt(&signer.key)
        .unwrap();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/mcp/github/connect?token={token}"))
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn callback_bad_state_returns_400() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("GET")
            .uri("/mcp/github/oauth/callback?code=abc&state=invalid.sig")
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn callback_provider_error_returns_400() {
        let app = make_state().await.router();
        let req = Request::builder()
            .method("GET")
            .uri("/mcp/github/oauth/callback?error=access_denied&error_description=User+denied")
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    fn resolved_for_test(resource: Option<&str>) -> super::ResolvedOAuth {
        super::ResolvedOAuth {
            authorization_endpoint: "https://example.com/authorize".to_string(),
            client_id: ClientId::new("client-1"),
            client_secret: None,
            redirect_uri: RedirectUri::new("https://coulisse.example.com/mcp/x/oauth/callback"),
            resource: resource.map(str::to_string),
            scopes: vec!["data:read_write".to_string()],
            token_endpoint: "https://example.com/token".to_string(),
        }
    }

    /// RFC 8707: when the MCP endpoint lives on a different origin than
    /// the auth server (Todoist's `ai.todoist.net` vs `todoist.com`), the
    /// authorize redirect must echo the MCP URL as `resource=` so the AS
    /// binds the issued token to it.
    #[test]
    fn authorize_url_includes_resource_when_present() {
        let resolved = resolved_for_test(Some("https://ai.todoist.net/mcp"));
        let url = resolved.authorize_url("STATE", &CodeChallenge::new("CHALLENGE"));
        assert!(
            url.contains("&resource=https%3A%2F%2Fai%2Etodoist%2Enet%2Fmcp"),
            "authorize URL missing url-encoded resource param: {url}"
        );
    }

    /// Static-config servers (legacy YAML) don't carry a resource URL;
    /// the redirect must be unchanged in that case.
    #[test]
    fn authorize_url_omits_resource_when_absent() {
        let resolved = resolved_for_test(None);
        let url = resolved.authorize_url("STATE", &CodeChallenge::new("CHALLENGE"));
        assert!(
            !url.contains("&resource="),
            "authorize URL should not carry resource= when unset: {url}"
        );
    }

    /// PKCE is mandatory per MCP OAuth 2.1 — every authorize URL must
    /// carry a `code_challenge` and declare S256. Without this, Todoist's
    /// resource-bound tokens fail to work against the MCP endpoint. Use
    /// a challenge made of alphanumerics so the assertion isn't fooled
    /// by percent-encoding of `-` / `_`.
    #[test]
    fn authorize_url_always_includes_pkce_challenge() {
        let resolved = resolved_for_test(Some("https://ai.todoist.net/mcp"));
        let url = resolved.authorize_url("STATE", &CodeChallenge::new("abc123XYZ"));
        assert!(
            url.contains("&code_challenge=abc123XYZ"),
            "authorize URL missing code_challenge: {url}"
        );
        assert!(
            url.contains("&code_challenge_method=S256"),
            "authorize URL missing code_challenge_method=S256: {url}"
        );
    }

    /// Scope priority: explicit YAML override wins, even when discovery
    /// would have offered something.
    #[test]
    fn yaml_scopes_override_discovered_ones() {
        let oauth = McpOAuthConfig::Discover {
            scopes: vec!["custom:scope".to_string()],
        };
        let discovered = vec!["data:read_write".to_string()];
        let effective = oauth.effective_scopes("server", &discovered);
        assert_eq!(effective, vec!["custom:scope".to_string()]);
    }

    /// When YAML pins no scopes, discovery (PRM/AS/DCR) wins.
    #[test]
    fn discovered_scopes_used_when_yaml_empty() {
        let oauth = McpOAuthConfig::Discover { scopes: vec![] };
        let discovered = vec!["data:read_write".to_string()];
        let effective = oauth.effective_scopes("server", &discovered);
        assert_eq!(effective, vec!["data:read_write".to_string()]);
    }

    /// Atlassian-style: nothing discovered, no YAML override. Falls
    /// back to OIDC defaults instead of empty. This is the behaviour
    /// `mcp-remote` uses for the same servers, and what unblocks the
    /// "MCP rejects empty-scope token" case.
    #[test]
    fn empty_yaml_and_empty_discovery_falls_back_to_oidc_defaults() {
        let oauth = McpOAuthConfig::Discover { scopes: vec![] };
        let effective = oauth.effective_scopes("server", &[]);
        assert_eq!(effective, vec!["openid", "email", "profile"]);
    }
}
