use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::config::McpServerConfig;
use crate::error::McpError;
use crate::oauth::{generate_state, validate_state};
use crate::vault::TokenVault;

#[derive(Clone)]
pub struct OAuthRouterState {
    pub configs: HashMap<String, McpServerConfig>,
    pub consumer_secret: String,
    pub hmac_key: Vec<u8>,
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
pub struct CallbackQuery {
    pub code: Option<String>,
    pub error: Option<String>,
    pub state: Option<String>,
}

/// Build the OAuth-related routes. Mounted outside proxy/admin auth
/// wrappers at the application root.
pub fn router(state: OAuthRouterState) -> Router {
    Router::new()
        .route("/mcp/{server}/connect-link", post(connect_link_handler))
        .route("/mcp/{server}/oauth/callback", get(oauth_callback_handler))
        .with_state(state)
}

async fn connect_link_handler(
    State(state): State<OAuthRouterState>,
    Path(server): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ConnectLinkQuery>,
) -> Response {
    // Validate consumer secret.
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {}", state.consumer_secret);
    if !constant_time_eq(auth.as_bytes(), expected.as_bytes()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Require user_id.
    let Some(user_id) = query.user_id.filter(|s| !s.is_empty()) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };

    // Server must exist and have oauth config.
    let config = match state.configs.get(&server) {
        Some(c) if c.oauth.is_some() => c,
        Some(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let oauth = config.oauth.as_ref().expect("checked above");

    let state_token = generate_state(&state.hmac_key, &server, &user_id);

    // Build authorization URL. Param order: client_id, redirect_uri,
    // response_type, scope (omitted when empty), state.
    let scopes = oauth.scopes.join(" ");
    let mut url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code",
        oauth.authorization_url,
        urlenccode(&oauth.client_id),
        urlenccode(&oauth.redirect_uri),
    );
    if !scopes.is_empty() {
        write!(url, "&scope={}", urlenccode(&scopes)).expect("write to String");
    }
    write!(url, "&state={}", urlenccode(&state_token)).expect("write to String");

    Json(ConnectLinkResponse { url }).into_response()
}

async fn oauth_callback_handler(
    State(state): State<OAuthRouterState>,
    Path(server): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    // If provider returned an error, surface it.
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

    let token_state = match validate_state(&state.hmac_key, &raw_state) {
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

    // State server must match path server.
    if token_state.server != server {
        return (StatusCode::BAD_REQUEST, Html(error_page("Server mismatch"))).into_response();
    }

    let config = match state.configs.get(&server) {
        Some(c) if c.oauth.is_some() => c,
        _ => {
            return (StatusCode::NOT_FOUND, Html(error_page("Unknown server"))).into_response();
        }
    };
    let oauth = config.oauth.as_ref().expect("checked above");

    // Exchange code for tokens.
    let exchange_result = exchange_code(oauth, &code).await;

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

    let expires_at = token_response
        .expires_in
        .map(|secs| coulisse_core::u64_to_i64(coulisse_core::now_secs() + secs));

    if let Err(e) = state
        .vault
        .upsert_token(
            &server,
            &token_state.user_id,
            &token_response.access_token,
            expires_at,
            token_response.refresh_token.as_deref(),
        )
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

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
}

async fn exchange_code(
    oauth: &crate::config::McpOAuthConfig,
    code: &str,
) -> Result<TokenResponse, McpError> {
    let client = reqwest::Client::new();
    let params = [
        ("client_id", oauth.client_id.as_str()),
        ("client_secret", oauth.client_secret.as_str()),
        ("code", code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", oauth.redirect_uri.as_str()),
    ];
    let response = client
        .post(&oauth.token_url)
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

fn urlenccode(s: &str) -> String {
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
    use http_body_util::BodyExt;
    use sqlx::sqlite::SqlitePoolOptions;
    use tower::ServiceExt;

    use super::{OAuthRouterState, router};
    use crate::config::{McpOAuthConfig, McpServerConfig, McpTransport};
    use crate::vault::TokenVault;

    const HMAC_KEY: &[u8] = b"test-hmac-key-32bytes-padding!!!";
    const SECRET: &str = "supersecret";

    async fn make_state() -> OAuthRouterState {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE mcp_oauth_tokens (\
                access_token_enc  BLOB    NOT NULL, \
                created_at        INTEGER NOT NULL, \
                expires_at        INTEGER, \
                refresh_token_enc BLOB, \
                server_name       TEXT    NOT NULL, \
                updated_at        INTEGER NOT NULL, \
                user_id           TEXT    NOT NULL, \
                PRIMARY KEY (server_name, user_id) \
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let vault_key = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        let vault = Arc::new(TokenVault::new(pool, &vault_key).unwrap());
        let oauth = McpOAuthConfig {
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
            consumer_secret: SECRET.into(),
            hmac_key: HMAC_KEY.to_vec(),
            vault,
        }
    }

    async fn call(app: axum::Router, req: Request<Body>) -> axum::response::Response {
        app.oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn connect_link_wrong_secret_returns_401() {
        let state = make_state().await;
        let app = router(state);
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
        let state = make_state().await;
        let app = router(state);
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
        let state = make_state().await;
        let app = router(state);
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
        let state = make_state().await;
        let app = router(state);
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
        let state = make_state().await;
        let app = router(state);
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
    async fn callback_bad_state_returns_400() {
        let state = make_state().await;
        let app = router(state);
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
        let state = make_state().await;
        let app = router(state);
        let req = Request::builder()
            .method("GET")
            .uri("/mcp/github/oauth/callback?error=access_denied&error_description=User+denied")
            .body(Body::empty())
            .unwrap();
        let resp = call(app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
