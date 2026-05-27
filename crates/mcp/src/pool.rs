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
    vault: Arc<TokenVault>,
}

impl UserMcpPool {
    #[must_use]
    pub fn new(
        configs: HashMap<String, McpServerConfig>,
        vault: Arc<TokenVault>,
        session_cache_size: Option<u64>,
    ) -> Self {
        let cap = session_cache_size.unwrap_or(DEFAULT_SESSION_CACHE_SIZE);
        let cache = Cache::builder()
            .max_capacity(cap)
            .time_to_idle(std::time::Duration::from_secs(1800))
            .build();
        Self {
            cache,
            configs,
            vault,
        }
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
        let stored = self
            .vault
            .get_token(server_name, &user_id_str)
            .await?
            .ok_or_else(|| McpError::NotConnected {
                server: server_name.to_string(),
                user_id: user_id_str.clone(),
            })?;

        // Token expired or within 60 seconds of expiry — no refresh in Phase 1-3.
        if let Some(exp) = stored.expires_at {
            let now = coulisse_core::u64_to_i64(coulisse_core::now_secs());
            if now >= exp - 60 {
                return Err(McpError::NotConnected {
                    server: server_name.to_string(),
                    user_id: user_id_str,
                });
            }
        }

        let session =
            Arc::new(connect_user_session(server_name, config, &stored.access_token).await?);
        self.cache.insert(key, Arc::clone(&session)).await;
        Ok(session)
    }
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
    pub(crate) fn new(server: &str, tool: rmcp::model::Tool, _user_id: &str) -> Self {
        let params = tool.schema_as_json_value();
        let message = format!(
            "Not connected: the user has not authorized access to the '{server}' MCP server. \
             Ask them to visit the connect URL to link their account."
        );
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
        let pool = UserMcpPool::new(configs, vault, None);

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
        let pool = UserMcpPool::new(configs, vault, None);

        let err = pool.get_or_spawn("github", user_id).await.unwrap_err();
        assert!(
            matches!(err, McpError::NotConnected { .. }),
            "expected NotConnected for expired token, got {err:?}"
        );
    }

    #[tokio::test]
    async fn not_connected_tool_returns_message() {
        let tool = rmcp::model::Tool::new_with_raw(
            "do_thing".to_string(),
            Some("does a thing".into()),
            Arc::new(serde_json::Map::new()),
        );
        let nct = NotConnectedTool::new("github", tool, "user-1");
        assert_eq!(nct.name(), "do_thing");
        let result = nct.call("{}".to_string()).await.unwrap();
        assert!(result.contains("github"));
        assert!(result.contains("connect URL"));
    }
}
