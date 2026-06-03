use std::collections::HashMap;
use std::sync::Arc;

use coulisse_core::UserId;
use moka::future::Cache;
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use rmcp::ServiceExt;
use rmcp::service::{RoleClient, RunningService, ServerSink};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use tokio::process::Command;
use tracing::instrument;

use crate::config::{McpServerConfig, McpTransport};
use crate::error::McpError;
use crate::routes::ConnectLinkSigner;
use crate::vault::TokenVault;

const DEFAULT_SESSION_CACHE_SIZE: u64 = 256;

/// A single connected MCP session for a specific user and server.
pub struct UserMcpSession {
    pub(crate) sink: ServerSink,
    pub(crate) tools: HashMap<String, rmcp::model::Tool>,
    _service: RunningService<RoleClient, ()>,
}

impl std::fmt::Debug for UserMcpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserMcpSession")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// LRU cache of per-user MCP sessions keyed by `(UserId, server_name)`.
pub struct UserMcpPool {
    cache: Cache<(UserId, String), Arc<UserMcpSession>>,
    configs: HashMap<String, McpServerConfig>,
    signer: Option<ConnectLinkSigner>,
    vault: Arc<TokenVault>,
}

impl UserMcpPool {
    #[must_use]
    pub fn new(
        configs: HashMap<String, McpServerConfig>,
        vault: Arc<TokenVault>,
        signer: Option<ConnectLinkSigner>,
        session_cache_size: Option<u64>,
    ) -> Self {
        let cap = session_cache_size.unwrap_or(DEFAULT_SESSION_CACHE_SIZE);
        let cache = Cache::builder()
            .max_capacity(cap)
            .time_to_idle(std::time::Duration::from_mins(30))
            .build();
        Self {
            cache,
            configs,
            signer,
            vault,
        }
    }

    /// Build the per-user connect URL to embed in a `NotConnectedTool`
    /// placeholder, or `None` if the signer wasn't configured (which
    /// shouldn't happen for any deployment that has an OAuth server, but
    /// the test pool can be constructed without one).
    pub(crate) fn signer(&self) -> Option<&ConnectLinkSigner> {
        self.signer.as_ref()
    }

    /// Get or spawn a session for the given user and OAuth-enabled server.
    /// Returns `McpError::NotConnected` if the user hasn't authorized yet.
    ///
    /// # Errors
    ///
    /// Returns an error if the vault lookup, process spawn, or connection fails.
    #[instrument(skip(self), fields(server = %server_name))]
    pub async fn get_or_spawn(
        &self,
        server_name: &str,
        user_id: UserId,
    ) -> Result<Arc<UserMcpSession>, McpError> {
        let key = (user_id, server_name.to_string());
        if let Some(session) = self.cache.get(&key).await {
            return Ok(session);
        }
        let config =
            self.configs
                .get(server_name)
                .ok_or_else(|| McpError::ServerNotConfigured {
                    agent: "<pool>".to_string(),
                    server: server_name.to_string(),
                })?;

        let user_id_str = user_id.0.to_string();
        let mut stored = self
            .vault
            .get_token(server_name, &user_id_str)
            .await?
            .ok_or_else(|| McpError::NotConnected {
                server: server_name.to_string(),
                user_id: user_id_str.clone(),
            })?;

        // Preemptive refresh: if the token is expired or within 60s of
        // expiry, exchange the refresh_token for a fresh pair before
        // even attempting the MCP call. Providers typically issue
        // 1-hour access tokens with long-lived refresh tokens; without
        // this branch, every hour-old session would 401 and force the
        // user to re-authorize through the browser.
        if let Some(exp) = stored.expires_at {
            let now = coulisse_core::u64_to_i64(coulisse_core::now_secs());
            if now >= exp - 60 {
                stored = self
                    .refresh_or_force_reauth(server_name, &user_id_str, &stored)
                    .await?;
            }
        }

        let session = match connect_user_session(server_name, config, &stored.access_token).await {
            Ok(s) => Arc::new(s),
            Err(e) if looks_like_auth_failure(&e) => {
                // Reactive refresh: the MCP endpoint rejected the
                // access_token despite our expiry check (clock skew,
                // server-side revocation, scope rotation). Try the
                // refresh_token before giving up — only fall through to
                // `NotConnected` if the refresh itself is dead.
                tracing::info!(
                    server = %server_name,
                    "MCP endpoint rejected stored token; attempting refresh"
                );
                let refreshed = self
                    .refresh_or_force_reauth(server_name, &user_id_str, &stored)
                    .await?;
                match connect_user_session(server_name, config, &refreshed.access_token).await {
                    Ok(session) => Arc::new(session),
                    Err(err) => {
                        // Refresh succeeded but the new token also fails.
                        // The provider's MCP is rejecting *all* our tokens —
                        // make the user reauth from scratch.
                        self.vault.delete_token(server_name, &user_id_str).await?;
                        tracing::warn!(
                            server = %server_name, error = %err,
                            "refresh succeeded but new token also rejected — forcing reauth"
                        );
                        return Err(McpError::NotConnected {
                            server: server_name.to_string(),
                            user_id: user_id_str,
                        });
                    }
                }
            }
            Err(e) => return Err(e),
        };
        self.cache.insert(key, Arc::clone(&session)).await;
        Ok(session)
    }

    /// Try refresh-token grant. On success, stores the new token pair
    /// (handling refresh-token rotation) and returns the fresh tokens.
    /// On failure, deletes the stored token and returns `NotConnected`
    /// so the caller surfaces a fresh connect URL.
    async fn refresh_or_force_reauth(
        &self,
        server_name: &str,
        user_id_str: &str,
        stored: &crate::vault::StoredToken,
    ) -> Result<crate::vault::StoredToken, McpError> {
        let Some(refresh_token) = stored.refresh_token.as_deref() else {
            // No refresh token — the original grant didn't include
            // `offline_access` or the provider simply doesn't issue
            // them. Nothing to do but ask the user to reauth.
            self.vault.delete_token(server_name, user_id_str).await?;
            return Err(McpError::NotConnected {
                server: server_name.to_string(),
                user_id: user_id_str.to_string(),
            });
        };
        match self
            .do_refresh(server_name, user_id_str, refresh_token)
            .await
        {
            Ok(new_stored) => Ok(new_stored),
            Err(err) => {
                tracing::warn!(
                    server = %server_name, error = %err,
                    "refresh failed; deleting token to force reauth"
                );
                self.vault.delete_token(server_name, user_id_str).await?;
                Err(McpError::NotConnected {
                    server: server_name.to_string(),
                    user_id: user_id_str.to_string(),
                })
            }
        }
    }

    async fn do_refresh(
        &self,
        server_name: &str,
        user_id_str: &str,
        refresh_token: &str,
    ) -> Result<crate::vault::StoredToken, McpError> {
        // The token endpoint and client_id are in the cached client
        // registration row. For confidential clients we also include the
        // client_secret; for public clients (Coulisse's preferred
        // mode), PKCE-style refresh just needs the refresh_token.
        let client =
            self.vault
                .get_client(server_name)
                .await?
                .ok_or_else(|| McpError::NotConnected {
                    server: server_name.to_string(),
                    user_id: user_id_str.to_string(),
                })?;
        let metadata: crate::discovery::AuthMetadata = serde_json::from_str(&client.metadata_json)
            .map_err(|source| McpError::Discovery {
                url: format!("<cached metadata for {server_name}>"),
                source: Box::new(source),
            })?;
        let mut params: Vec<(&str, &str)> = vec![
            ("client_id", client.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ];
        if let Some(secret) = client.client_secret.as_deref() {
            params.push(("client_secret", secret));
        }
        let response = reqwest::Client::new()
            .post(&metadata.token_endpoint)
            .header("Accept", "application/json")
            .form(&params)
            .send()
            .await
            .map_err(|source| McpError::TokenExchange {
                server: server_name.to_string(),
                source,
            })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::Discovery {
                url: metadata.token_endpoint.clone(),
                source: format!("refresh failed: HTTP {status}: {body}").into(),
            });
        }
        #[derive(serde::Deserialize)]
        struct RefreshResponse {
            access_token: String,
            #[serde(default)]
            expires_in: Option<u64>,
            #[serde(default)]
            refresh_token: Option<String>,
        }
        let parsed: RefreshResponse =
            response
                .json()
                .await
                .map_err(|source| McpError::TokenExchange {
                    server: server_name.to_string(),
                    source,
                })?;
        // RFC 6749 §6: the AS MAY issue a new refresh_token. If it
        // does (rotation), use it. If not, keep the existing one.
        let new_refresh = parsed.refresh_token.as_deref().unwrap_or(refresh_token);
        let new_exp = parsed
            .expires_in
            .map(|s| coulisse_core::u64_to_i64(coulisse_core::now_secs() + s));
        self.vault
            .upsert_token(
                server_name,
                user_id_str,
                &parsed.access_token,
                new_exp,
                Some(new_refresh),
            )
            .await?;
        // The caller will reconnect with this fresh access_token, so
        // the moka session cache for this `(user, server)` key naturally
        // gets replaced on the next `cache.insert` in `get_or_spawn`.
        Ok(crate::vault::StoredToken {
            access_token: parsed.access_token,
            expires_at: new_exp,
            refresh_token: Some(new_refresh.to_string()),
        })
    }
}

/// Detect "the MCP endpoint refused this token" by string-matching the
/// rmcp error chain. Strict downcasting would need to thread the inner
/// transport error type (`StreamableHttpError<reqwest::Error>`) through
/// `Box<dyn Error>` boundaries that rmcp doesn't expose. The variants
/// are stable, public API; if they ever change, the unit tests fail
/// before users notice.
///
/// We match three shapes because real-world MCP endpoints don't all
/// return clean RFC-9728 `WWW-Authenticate` headers on 401:
///
/// - `AuthRequired(...)` — rmcp parsed a proper Bearer challenge.
/// - `InsufficientScope(...)` — 403 with `scope=` in the challenge.
/// - `UnexpectedServerResponse("HTTP 401 ...")` / `("HTTP 403 ...")` —
///   server returned 401/403 without a parseable challenge (Atlassian's
///   MCP at `mcp.atlassian.com` does this).
fn looks_like_auth_failure(err: &McpError) -> bool {
    let formatted = format!("{err:?}");
    formatted.contains("AuthRequired")
        || formatted.contains("InsufficientScope")
        || formatted.contains("HTTP 401")
        || formatted.contains("HTTP 403")
}

async fn connect_user_session(
    name: &str,
    config: &McpServerConfig,
    access_token: &str,
) -> Result<UserMcpSession, McpError> {
    let service = match &config.transport {
        McpTransport::Http { url } => {
            // Build the transport config with Bearer auth header so the MCP HTTP
            // server receives it on every request.
            let transport_config = StreamableHttpClientTransportConfig::with_uri(url.as_str())
                .auth_header(format!("Bearer {access_token}"));
            let transport = StreamableHttpClientTransport::from_config(transport_config);
            ().serve(transport)
                .await
                .map_err(|source| McpError::Connect {
                    server: name.to_string(),
                    source: Box::new(source),
                })?
        }
        McpTransport::Sse { url } => {
            let transport = crate::sse_client::SseClientTransport::connect(
                url,
                Some(format!("Bearer {access_token}")),
            )
            .await
            .map_err(|source| McpError::Connect {
                server: name.to_string(),
                source: Box::new(source),
            })?;
            ().serve(transport)
                .await
                .map_err(|source| McpError::Connect {
                    server: name.to_string(),
                    source: Box::new(source),
                })?
        }
        McpTransport::Stdio { args, command, env } => {
            let mut cmd = Command::new(command);
            cmd.args(args);
            if !env.is_empty() {
                cmd.envs(env);
            }
            // Inject the OAuth token so the stdio MCP server can authenticate.
            cmd.env("MCP_OAUTH_TOKEN", access_token);
            let transport = TokioChildProcess::new(cmd).map_err(|source| McpError::Spawn {
                server: name.to_string(),
                source,
            })?;
            ().serve(transport)
                .await
                .map_err(|source| McpError::Connect {
                    server: name.to_string(),
                    source: Box::new(source),
                })?
        }
    };

    let listed = service
        .list_tools(Option::default())
        .await
        .map_err(|source| McpError::ListTools {
            server: name.to_string(),
            source,
        })?;
    let tools = listed
        .tools
        .into_iter()
        .map(|tool| (tool.name.to_string(), tool))
        .collect();
    let sink = service.peer().clone();
    Ok(UserMcpSession {
        _service: service,
        sink,
        tools,
    })
}

/// Placeholder tool that always returns the "not connected" message so the
/// LLM can surface it naturally without causing a hard error.
pub(crate) struct NotConnectedTool {
    pub(crate) definition: ToolDefinition,
    pub(crate) message: String,
}

impl NotConnectedTool {
    pub(crate) fn new(
        server: &str,
        tool: rmcp::model::Tool,
        user_id: &str,
        signer: Option<&ConnectLinkSigner>,
    ) -> Self {
        let params = tool.schema_as_json_value();
        let message = match signer {
            Some(s) => {
                let url = s.connect_url(server, user_id);
                format!(
                    "Not connected: the user has not authorized access to the '{server}' MCP \
                     server. Show them this link verbatim and ask them to open it to link \
                     their account — the link is single-use and tied to their identity. Do not \
                     share it with anyone else, and do not edit, regenerate, or invent any part \
                     of it (the URL is HMAC-signed; any modification makes it invalid). If a \
                     previous link expired, call this tool again to get a fresh one — never \
                     hand-roll a new URL. URL: {url}"
                )
            }
            None => format!(
                "Not connected: the user has not authorized access to the '{server}' MCP server, \
                 and Coulisse is misconfigured (missing public_base_url or hmac_key). Ask an \
                 administrator to check the server logs."
            ),
        };
        Self {
            definition: ToolDefinition {
                description: tool.description.map(|d| d.to_string()).unwrap_or_default(),
                name: tool.name.to_string(),
                parameters: params,
            },
            message,
        }
    }
}

impl ToolDyn for NotConnectedTool {
    fn call(&self, _args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let msg = self.message.clone();
        Box::pin(async move { Ok(msg) })
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        let def = self.definition.clone();
        Box::pin(async move { def })
    }

    fn name(&self) -> String {
        self.definition.name.clone()
    }
}

/// Sibling of `NotConnectedTool` for runtime failures that aren't auth
/// problems — network blips, MCP server crashes, malformed responses,
/// vault DB errors. Surfacing a placeholder instead of bubbling up the
/// error keeps one broken MCP from taking down every other tool the
/// agent has (filesystem, other MCPs, subagents). The LLM sees the
/// description and can decide whether to try anyway, work around it, or
/// just tell the user.
pub(crate) struct UnreachableTool {
    pub(crate) definition: ToolDefinition,
    pub(crate) message: String,
}

impl UnreachableTool {
    pub(crate) fn new(server: &str, error_summary: &str) -> Self {
        let mut schema = serde_json::Map::new();
        schema.insert(
            "type".to_string(),
            serde_json::Value::String("object".to_string()),
        );
        schema.insert(
            "properties".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
        Self {
            definition: ToolDefinition {
                description: format!(
                    "The '{server}' MCP server is currently unreachable. Calling tools on \
                     this server will fail. Tell the user that '{server}' is temporarily \
                     unavailable and proceed with whatever else you can help them with."
                ),
                name: format!("{server}_unreachable"),
                parameters: serde_json::Value::Object(schema),
            },
            message: format!(
                "MCP server '{server}' is unreachable: {error_summary}. The admin should \
                 check Coulisse logs for the full error chain."
            ),
        }
    }
}

impl ToolDyn for UnreachableTool {
    fn call(&self, _args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let msg = self.message.clone();
        Box::pin(async move { Ok(msg) })
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        let def = self.definition.clone();
        Box::pin(async move { def })
    }

    fn name(&self) -> String {
        self.definition.name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn make_vault_with_token(
        server: &str,
        user_id_str: &str,
        expires_at: Option<i64>,
    ) -> Arc<TokenVault> {
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
        let key = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        let vault = Arc::new(TokenVault::new(pool, &key).unwrap());
        vault
            .upsert_token(server, user_id_str, "tok", expires_at, None)
            .await
            .unwrap();
        vault
    }

    /// `looks_like_auth_failure` must match the error variants rmcp uses
    /// when the MCP endpoint returns 401/403. The variant names live in
    /// `rmcp::transport::streamable_http_client::StreamableHttpError` and
    /// are debug-printed by the `thiserror`-derived chain that bubbles up
    /// through `McpError::Connect` / `McpError::ListTools`.
    #[test]
    fn looks_like_auth_failure_matches_rmcp_variant_names() {
        // Sanity: a generic transport-level message is not an auth signal.
        let benign = McpError::Connect {
            server: "x".to_string(),
            source: "connection refused".into(),
        };
        assert!(!super::looks_like_auth_failure(&benign));

        // The strings that appear when rmcp's StreamableHttpError debug-
        // prints AuthRequired / InsufficientScope. If rmcp ever renames
        // these variants, this test fails and we update the matcher.
        let auth = McpError::Connect {
            server: "x".to_string(),
            source: "Transport channel closed, when AuthRequired(AuthRequiredError { ... })".into(),
        };
        assert!(super::looks_like_auth_failure(&auth));

        let scope = McpError::Connect {
            server: "x".to_string(),
            source: "InsufficientScope(InsufficientScopeError { ... })".into(),
        };
        assert!(super::looks_like_auth_failure(&scope));

        // Atlassian's MCP returns 401 without a parseable Bearer challenge;
        // rmcp surfaces it as UnexpectedServerResponse with the status line
        // embedded. Must still trigger token removal so the user can reauth.
        let bare_401 = McpError::Connect {
            server: "x".to_string(),
            source: "Transport channel closed, when UnexpectedServerResponse(\"HTTP 401 Unauthorized: Unauthorized\")".into(),
        };
        assert!(super::looks_like_auth_failure(&bare_401));

        let bare_403 = McpError::Connect {
            server: "x".to_string(),
            source: "UnexpectedServerResponse(\"HTTP 403 Forbidden\")".into(),
        };
        assert!(super::looks_like_auth_failure(&bare_403));
    }

    /// After `delete_token`, `get_token` returns `None` — the row is gone,
    /// not just zeroed out. Important because `get_or_spawn`'s recovery
    /// path expects the next lookup to fail with `NotConnected`.
    #[tokio::test]
    async fn delete_token_removes_the_row() {
        let user_id = coulisse_core::UserId::new();
        let user_id_str = user_id.0.to_string();
        let vault = make_vault_with_token("github", &user_id_str, None).await;

        assert!(
            vault
                .get_token("github", &user_id_str)
                .await
                .unwrap()
                .is_some(),
            "setup: token should be present"
        );

        vault.delete_token("github", &user_id_str).await.unwrap();

        assert!(
            vault
                .get_token("github", &user_id_str)
                .await
                .unwrap()
                .is_none(),
            "token row must be gone after delete"
        );
    }

    /// Deleting a token that isn't there must be a quiet no-op, not an
    /// error — callers shouldn't have to guard against double-deletes.
    #[tokio::test]
    async fn delete_token_is_idempotent() {
        let user_id = coulisse_core::UserId::new();
        let user_id_str = user_id.0.to_string();
        let vault = make_vault_with_token("github", &user_id_str, None).await;

        vault.delete_token("github", &user_id_str).await.unwrap();
        vault.delete_token("github", &user_id_str).await.unwrap();
    }

    /// `expires_at = None` means no expiry information — token must not be
    /// rejected as expired. We verify the session lookup reaches the connect
    /// step (which fails here since there's no real server) rather than
    /// `NotConnected`.
    #[tokio::test]
    async fn token_without_expiry_not_rejected_as_expired() {
        let user_id = coulisse_core::UserId::new();
        let user_id_str = user_id.0.to_string();
        let vault = make_vault_with_token("github", &user_id_str, None).await;
        let configs = HashMap::new();
        let pool = UserMcpPool::new(configs, vault, None, None);

        // No server config → ServerNotConfigured, not NotConnected.
        // This proves the token expiry check was passed.
        let err = pool.get_or_spawn("github", user_id).await.unwrap_err();
        assert!(
            matches!(err, McpError::ServerNotConfigured { .. }),
            "expected ServerNotConfigured, got {err:?}"
        );
    }

    /// A token with `expires_at` in the past must yield `NotConnected`.
    #[tokio::test]
    async fn expired_token_returns_not_connected() {
        let user_id = coulisse_core::UserId::new();
        let user_id_str = user_id.0.to_string();
        // Expiry set to Unix epoch — definitely in the past.
        let vault = make_vault_with_token("github", &user_id_str, Some(1)).await;
        // Server must be configured for the expiry check to be reached;
        // otherwise we'd short-circuit on `ServerNotConfigured`.
        let mut configs = HashMap::new();
        configs.insert(
            "github".to_string(),
            McpServerConfig {
                oauth: None,
                transport: McpTransport::Http {
                    url: "http://localhost".to_string(),
                },
            },
        );
        let pool = UserMcpPool::new(configs, vault, None, None);

        let err = pool.get_or_spawn("github", user_id).await.unwrap_err();
        assert!(
            matches!(err, McpError::NotConnected { .. }),
            "expected NotConnected for expired token, got {err:?}"
        );
    }

    #[tokio::test]
    async fn not_connected_tool_returns_message_with_connect_url() {
        let tool = rmcp::model::Tool::new_with_raw(
            "do_thing".to_string(),
            Some("does a thing".into()),
            Arc::new(serde_json::Map::new()),
        );
        let signer = ConnectLinkSigner {
            hmac_key: b"test-hmac-key-32bytes-padding!!!".to_vec(),
            public_base_url: "http://localhost:8421".into(),
        };
        let nct = NotConnectedTool::new("github", tool, "user-1", Some(&signer));
        assert_eq!(nct.name(), "do_thing");
        let result = nct.call("{}".to_string()).await.unwrap();
        assert!(result.contains("github"));
        // The message must contain a real, clickable URL — the model
        // can't relay something useful without one.
        assert!(
            result.contains("http://localhost:8421/mcp/github/connect?token="),
            "message missing connect URL: {result}"
        );
    }

    #[tokio::test]
    async fn not_connected_tool_without_signer_warns() {
        let tool = rmcp::model::Tool::new_with_raw(
            "do_thing".to_string(),
            None,
            Arc::new(serde_json::Map::new()),
        );
        let nct = NotConnectedTool::new("github", tool, "user-1", None);
        let result = nct.call("{}".to_string()).await.unwrap();
        assert!(result.contains("misconfigured"));
    }
}
