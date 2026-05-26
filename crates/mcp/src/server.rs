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
use crate::pool::{NotConnectedTool, UserMcpPool};
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
        Self::connect_with_vault(configs, None).await
    }

    /// Connect with an optional vault for OAuth-enabled servers.
    ///
    /// # Errors
    ///
    /// Returns an error if any non-OAuth server connection fails.
    pub async fn connect_with_vault(
        configs: HashMap<String, McpServerConfig>,
        vault: Option<Arc<TokenVault>>,
    ) -> Result<Self, McpError> {
        let mut servers = HashMap::new();
        for (name, cfg) in &configs {
            if cfg.oauth.is_none() {
                let server = McpServer::connect(name, cfg).await?;
                servers.insert(name.clone(), server);
            }
        }
        let user_pool = vault.map(|v| Arc::new(UserMcpPool::new(configs.clone(), v, None)));
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
            let config = self.configs.get(&access.server).ok_or_else(|| {
                McpError::ServerNotConfigured {
                    agent: agent.to_string(),
                    server: access.server.clone(),
                }
            })?;
            if config.oauth.is_some() {
                // OAuth servers are not accessible without a user_id.
                return Err(McpError::ServerNotConfigured {
                    agent: agent.to_string(),
                    server: access.server.clone(),
                });
            }
            let server = self
                .servers
                .get(&access.server)
                .ok_or_else(|| McpError::ServerNotConfigured {
                    agent: agent.to_string(),
                    server: access.server.clone(),
                })?;
            let picked = pick_tools(agent, &access.server, &access.only, &server.tools)?;
            for tool in picked {
                tools.push(Box::new(McpTool::from_mcp_server(tool, server.sink.clone())));
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
        for access in accesses {
            let config = self.configs.get(&access.server).ok_or_else(|| {
                McpError::ServerNotConfigured {
                    agent: agent.to_string(),
                    server: access.server.clone(),
                }
            })?;

            if config.oauth.is_some() {
                let pool = self.user_pool.as_ref().ok_or_else(|| {
                    McpError::ServerNotConfigured {
                        agent: agent.to_string(),
                        server: access.server.clone(),
                    }
                })?;

                match pool.get_or_spawn(&access.server, user_id).await {
                    Ok(session) => {
                        let picked =
                            pick_tools(agent, &access.server, &access.only, &session.tools)?;
                        for tool in picked {
                            tools.push(Box::new(McpTool::from_mcp_server(
                                tool,
                                session.sink.clone(),
                            )));
                        }
                    }
                    Err(McpError::NotConnected { server, user_id: uid }) => {
                        // Surface as not-connected placeholder tools.
                        let server_tools: Vec<rmcp::model::Tool> = match &access.only {
                            None => vec![],
                            Some(names) => names
                                .iter()
                                .map(|n| {
                                    rmcp::model::Tool::new_with_raw(
                                        n.clone(),
                                        None,
                                        Arc::new(serde_json::Map::new()),
                                    )
                                })
                                .collect(),
                        };
                        for tool in server_tools {
                            tools.push(Box::new(NotConnectedTool::new(&server, tool, &uid)));
                        }
                    }
                    Err(e) => return Err(e),
                }
            } else {
                let server = self
                    .servers
                    .get(&access.server)
                    .ok_or_else(|| McpError::ServerNotConfigured {
                        agent: agent.to_string(),
                        server: access.server.clone(),
                    })?;
                let picked = pick_tools(agent, &access.server, &access.only, &server.tools)?;
                for tool in picked {
                    tools.push(Box::new(McpTool::from_mcp_server(tool, server.sink.clone())));
                }
            }
        }
        Ok(sanitize::apply(tools))
    }
}

fn pick_tools(
    agent: &str,
    server_name: &str,
    only: &Option<Vec<String>>,
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
            McpTransport::Stdio { args, command, env } => {
                let mut cmd = Command::new(command);
                cmd.args(args);
                if !env.is_empty() {
                    cmd.envs(env);
                }
                let transport =
                    TokioChildProcess::new(cmd).map_err(|source| McpError::Spawn {
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
