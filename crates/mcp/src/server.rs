use std::collections::HashMap;
use std::sync::Arc;

use coulisse_core::UserId;
use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::ServiceExt;
use rmcp::service::{RoleClient, RunningService, ServerSink};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use tokio::process::Command;

use crate::config::{McpServerConfig, McpToolAccess, McpTransport};
use crate::error::McpError;
use crate::pool::{NotConnectedTool, UnreachableTool, UserMcpPool};
use crate::routes::ConnectLinkSigner;
use crate::sanitize;
use crate::vault::TokenVault;

/// Pool of connected MCP servers for non-OAuth servers, keyed by YAML name.
/// OAuth-enabled servers use `UserMcpPool` instead (per-user sessions).
pub struct McpServers {
    configs: HashMap<String, McpServerConfig>,
    servers: HashMap<String, McpServer>,
    user_pool: Option<Arc<UserMcpPool>>,
}

struct McpServer {
    _service: RunningService<RoleClient, ()>,
    sink: ServerSink,
    tools: HashMap<String, rmcp::model::Tool>,
}

impl McpServers {
    /// Connect to every non-OAuth server in `configs` at boot. OAuth servers
    /// are handled lazily via `UserMcpPool` on first use.
    ///
    /// # Errors
    ///
    /// Returns an error if any non-OAuth server connection fails.
    pub async fn connect(configs: HashMap<String, McpServerConfig>) -> Result<Self, McpError> {
        Self::connect_with_vault(configs, None, None).await
    }

    /// Connect with an optional vault for OAuth-enabled servers. The
    /// `signer` is used to mint the per-user connect URLs that
    /// `NotConnectedTool` surfaces to the LLM — required for OAuth-enabled
    /// servers (every deployment with one) and harmless to leave `None`
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error if any non-OAuth server connection fails.
    pub async fn connect_with_vault(
        configs: HashMap<String, McpServerConfig>,
        vault: Option<Arc<TokenVault>>,
        signer: Option<ConnectLinkSigner>,
    ) -> Result<Self, McpError> {
        let mut servers = HashMap::new();
        // 20 seconds is long enough for a healthy stdio MCP child to
        // initialize even on a cold-start machine, and short enough that
        // a broken or blocked child (the canonical case: `mcp-remote`
        // hanging on browser auth) doesn't deadlock the entire HTTP
        // server's startup. Children that time out continue running and
        // can be retried on the next chat turn — the agent just sees a
        // `<server>_unreachable` placeholder until then.
        const PER_SERVER_INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
        for (name, cfg) in &configs {
            if cfg.oauth.is_none() {
                match tokio::time::timeout(PER_SERVER_INIT_TIMEOUT, McpServer::connect(name, cfg))
                    .await
                {
                    Ok(Ok(server)) => {
                        servers.insert(name.clone(), server);
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            server = %name, error = %e,
                            "MCP server failed to initialize at boot; \
                             agents will see an _unreachable placeholder \
                             until the next successful retry"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            server = %name,
                            timeout_secs = PER_SERVER_INIT_TIMEOUT.as_secs(),
                            "MCP server didn't respond within timeout (likely \
                             blocked on stdin/auth). Continuing without it; the \
                             child process keeps running and may be retried later."
                        );
                    }
                }
            }
        }
        let user_pool = vault.map(|v| Arc::new(UserMcpPool::new(configs.clone(), v, signer, None)));
        Ok(Self {
            configs,
            servers,
            user_pool,
        })
    }

    /// Build tools for a non-OAuth agent. Panics if called for an
    /// OAuth-enabled server (use `tools_for_user` instead).
    ///
    /// # Errors
    ///
    /// Returns an error if a referenced server or tool is not found.
    pub fn tools_for(
        &self,
        agent: &str,
        accesses: &[McpToolAccess],
    ) -> Result<Vec<Box<dyn ToolDyn>>, McpError> {
        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
        for access in accesses {
            let config =
                self.configs
                    .get(&access.server)
                    .ok_or_else(|| McpError::ServerNotConfigured {
                        agent: agent.to_string(),
                        server: access.server.clone(),
                    })?;
            if config.oauth.is_some() {
                // OAuth servers are not accessible without a user_id.
                return Err(McpError::ServerNotConfigured {
                    agent: agent.to_string(),
                    server: access.server.clone(),
                });
            }
            let server =
                self.servers
                    .get(&access.server)
                    .ok_or_else(|| McpError::ServerNotConfigured {
                        agent: agent.to_string(),
                        server: access.server.clone(),
                    })?;
            let picked = pick_tools(agent, &access.server, access.only.as_deref(), &server.tools)?;
            for tool in picked {
                tools.push(Box::new(McpTool::from_mcp_server(
                    tool,
                    server.sink.clone(),
                )));
            }
        }
        Ok(sanitize::apply(tools))
    }

    /// Build tools for a specific user. OAuth-enabled servers look up the
    /// vault and return `NotConnectedTool` instances when no token is stored.
    ///
    /// # Errors
    ///
    /// Returns an error if a referenced server or tool is not found, or if
    /// vault access fails.
    pub async fn tools_for_user(
        &self,
        agent: &str,
        accesses: &[McpToolAccess],
        user_id: UserId,
    ) -> Result<Vec<Box<dyn ToolDyn>>, McpError> {
        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
        // Per-server isolation: if one server is broken (network down,
        // crash, vault DB hiccup), the agent still gets every other
        // server's tools. One bad MCP must not deny the user every
        // other capability — that's what turned every Atlassian/Todoist
        // misconfiguration into a 502 wall for the whole chat.
        for access in accesses {
            let Some(config) = self.configs.get(&access.server) else {
                tracing::warn!(
                    agent = %agent, server = %access.server,
                    "agent references MCP server that is not in the `mcp:` config block"
                );
                tools.push(Box::new(UnreachableTool::new(
                    &access.server,
                    "server not configured",
                )));
                continue;
            };

            if config.oauth.is_some() {
                let Some(pool) = self.user_pool.as_ref() else {
                    tracing::warn!(
                        agent = %agent, server = %access.server,
                        "oauth server referenced but no user pool — vault is misconfigured"
                    );
                    tools.push(Box::new(UnreachableTool::new(
                        &access.server,
                        "no OAuth vault",
                    )));
                    continue;
                };

                match pool.get_or_spawn(&access.server, user_id).await {
                    Ok(session) => {
                        match pick_tools(
                            agent,
                            &access.server,
                            access.only.as_deref(),
                            &session.tools,
                        ) {
                            Ok(picked) => {
                                for tool in picked {
                                    tools.push(Box::new(McpTool::from_mcp_server(
                                        tool,
                                        session.sink.clone(),
                                    )));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    agent = %agent, server = %access.server, error = %e,
                                    "could not pick tools from MCP session"
                                );
                                tools.push(Box::new(UnreachableTool::new(
                                    &access.server,
                                    &e.to_string(),
                                )));
                            }
                        }
                    }
                    Err(McpError::NotConnected {
                        server,
                        user_id: uid,
                    }) => {
                        let placeholders: Vec<rmcp::model::Tool> = match &access.only {
                            None => vec![sentinel_placeholder(&server)],
                            Some(names) => names.iter().map(|n| named_placeholder(n)).collect(),
                        };
                        let signer = pool.signer();
                        for tool in placeholders {
                            tools
                                .push(Box::new(NotConnectedTool::new(&server, tool, &uid, signer)));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent = %agent, server = %access.server, error = %e,
                            "MCP session setup failed; exposing unreachable placeholder"
                        );
                        tools.push(Box::new(UnreachableTool::new(
                            &access.server,
                            &e.to_string(),
                        )));
                    }
                }
            } else {
                let Some(server) = self.servers.get(&access.server) else {
                    tracing::warn!(
                        agent = %agent, server = %access.server,
                        "non-OAuth MCP server is not in the connected pool \
                         (boot connect failed?)"
                    );
                    tools.push(Box::new(UnreachableTool::new(
                        &access.server,
                        "server not connected at boot",
                    )));
                    continue;
                };
                match pick_tools(agent, &access.server, access.only.as_deref(), &server.tools) {
                    Ok(picked) => {
                        for tool in picked {
                            tools.push(Box::new(McpTool::from_mcp_server(
                                tool,
                                server.sink.clone(),
                            )));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent = %agent, server = %access.server, error = %e,
                            "could not pick tools from MCP server"
                        );
                        tools.push(Box::new(UnreachableTool::new(
                            &access.server,
                            &e.to_string(),
                        )));
                    }
                }
            }
        }
        Ok(sanitize::apply(tools))
    }
}

/// JSON Schema for placeholder tools that take no arguments. Anthropic
/// rejects a bare `{}` (`400 tools.0.custom.input_schema.type: Field
/// required`), so we emit `{"type": "object", "properties": {}}`.
fn empty_object_schema() -> Arc<serde_json::Map<String, serde_json::Value>> {
    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );
    schema.insert(
        "properties".to_string(),
        serde_json::Value::Object(serde_json::Map::new()),
    );
    Arc::new(schema)
}

/// Single placeholder for an OAuth-pending server whose tool schema is
/// unknown (no `only:` list and no user has authorised yet). The
/// description makes it obvious to the LLM that this is the
/// authorisation entry point, not a real tool, AND that the URL it
/// returns must be relayed verbatim — LLMs given a structured URL with
/// an `exp` field will sometimes "refresh" it in prose, producing a
/// forged signature that fails HMAC validation.
fn sentinel_placeholder(server: &str) -> rmcp::model::Tool {
    rmcp::model::Tool::new_with_raw(
        format!("connect_{server}"),
        Some(
            format!(
                "Returns a one-time URL the user must open to authorize access to the \
                 '{server}' MCP server. Call this whenever the user wants to use \
                 {server} features or asks you to connect to {server}, INCLUDING when a \
                 previous link has expired — always call the tool to get a fresh URL. \
                 Copy the URL from the tool result into your reply verbatim; never edit \
                 it, regenerate it, or invent a new one (the URL is HMAC-signed and any \
                 modification makes it invalid)."
            )
            .into(),
        ),
        empty_object_schema(),
    )
}

/// Placeholder tool that mirrors a name from the agent's `only:` list.
/// The schema is a stand-in — calling the tool returns the connect URL
/// rather than executing anything.
fn named_placeholder(name: &str) -> rmcp::model::Tool {
    rmcp::model::Tool::new_with_raw(name.to_string(), None, empty_object_schema())
}

fn pick_tools(
    agent: &str,
    server_name: &str,
    only: Option<&[String]>,
    available: &HashMap<String, rmcp::model::Tool>,
) -> Result<Vec<rmcp::model::Tool>, McpError> {
    match only {
        None => Ok(available.values().cloned().collect()),
        Some(names) => names
            .iter()
            .map(|name| {
                available
                    .get(name)
                    .cloned()
                    .ok_or_else(|| McpError::ToolNotFound {
                        agent: agent.to_string(),
                        server: server_name.to_string(),
                        tool: name.clone(),
                    })
            })
            .collect::<Result<_, _>>(),
    }
}

impl McpServer {
    async fn connect(name: &str, config: &McpServerConfig) -> Result<Self, McpError> {
        let service = match &config.transport {
            McpTransport::Http { url } => {
                let transport = StreamableHttpClientTransport::from_uri(url.clone());
                ().serve(transport)
                    .await
                    .map_err(|source| McpError::Connect {
                        server: name.to_string(),
                        source: Box::new(source),
                    })?
            }
            McpTransport::Sse { url } => {
                let transport = crate::sse_client::SseClientTransport::connect(url, None)
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
        Ok(Self {
            _service: service,
            sink,
            tools,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpOAuthConfig;
    use base64::Engine as _;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn sentinel_placeholder_advertises_connect_name_and_description() {
        let tool = sentinel_placeholder("todoist");
        assert_eq!(tool.name.as_ref(), "connect_todoist");
        let desc = tool
            .description
            .as_ref()
            .expect("sentinel must carry a description")
            .as_ref();
        assert!(desc.contains("todoist"), "description: {desc}");
        assert!(
            desc.contains("authorize") || desc.contains("connect"),
            "description should point the LLM at the auth flow: {desc}"
        );
    }

    #[test]
    fn named_placeholder_preserves_caller_supplied_name() {
        let tool = named_placeholder("create_task");
        assert_eq!(tool.name.as_ref(), "create_task");
    }

    /// Without `only:` and without a stored token, the agent must still
    /// see exactly one tool — the sentinel — so the LLM can call it and
    /// receive the connect URL. The pre-fix behavior was zero tools,
    /// which left the LLM to confabulate empty results.
    #[tokio::test]
    async fn no_only_no_token_surfaces_one_sentinel_tool() {
        let user_id = UserId::new();
        let mut configs = HashMap::new();
        configs.insert(
            "todoist".to_string(),
            McpServerConfig {
                no_rewrite: false,
                oauth: Some(McpOAuthConfig::Discover { scopes: vec![] }),
                transport: McpTransport::Http {
                    url: "https://ai.todoist.net/mcp".to_string(),
                },
            },
        );

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
        let signer = ConnectLinkSigner {
            hmac_key: b"test-hmac-key-32bytes-padding!!!".to_vec(),
            public_base_url: "http://localhost:8421".into(),
        };

        let servers = McpServers::connect_with_vault(configs, Some(vault), Some(signer))
            .await
            .unwrap();

        let access = McpToolAccess {
            only: None,
            server: "todoist".to_string(),
        };
        let tools = servers
            .tools_for_user("pm", std::slice::from_ref(&access), user_id)
            .await
            .unwrap();

        assert_eq!(
            tools.len(),
            1,
            "expected exactly one sentinel tool, got {}",
            tools.len()
        );
        assert_eq!(tools[0].name(), "connect_todoist");
    }

    /// An agent references an MCP server that isn't declared under
    /// `mcp:` at all. The other servers in the agent's list must still
    /// appear; the missing one degrades to `<server>_unreachable`.
    /// Pre-fix this returned an error and the whole chat 502'd.
    #[tokio::test]
    async fn missing_server_does_not_crash_the_agent() {
        let user_id = UserId::new();
        // Only `present` is configured; the agent will also ask for
        // `missing`, which doesn't exist in the config.
        let mut configs = HashMap::new();
        configs.insert(
            "present".to_string(),
            McpServerConfig {
                no_rewrite: false,
                oauth: Some(McpOAuthConfig::Discover { scopes: vec![] }),
                transport: McpTransport::Http {
                    url: "https://example.com/mcp".to_string(),
                },
            },
        );

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
        let signer = ConnectLinkSigner {
            hmac_key: b"test-hmac-key-32bytes-padding!!!".to_vec(),
            public_base_url: "http://localhost:8421".into(),
        };

        let servers = McpServers::connect_with_vault(configs, Some(vault), Some(signer))
            .await
            .unwrap();

        let accesses = vec![
            McpToolAccess {
                only: None,
                server: "missing".to_string(),
            },
            McpToolAccess {
                only: None,
                server: "present".to_string(),
            },
        ];
        let tools = servers
            .tools_for_user("pm", &accesses, user_id)
            .await
            .expect("missing server must not propagate an error to the caller");

        // One placeholder for the missing server, one sentinel for the
        // present-but-unauthorized server. Total 2.
        assert_eq!(tools.len(), 2, "got tool names: {:?}", names(&tools));
        let names: Vec<String> = tools.iter().map(|t| t.name()).collect();
        assert!(
            names.iter().any(|n| n == "missing_unreachable"),
            "expected missing_unreachable placeholder, got {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "connect_present"),
            "expected connect_present sentinel, got {names:?}"
        );
    }

    fn names(tools: &[Box<dyn ToolDyn>]) -> Vec<String> {
        tools.iter().map(|t| t.name()).collect()
    }
}
