use std::collections::HashMap;

use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::ServiceExt;
use rmcp::service::{RoleClient, RunningService, ServerSink};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use tokio::process::Command;

use crate::config::{McpServerConfig, McpToolAccess};
use crate::error::McpError;

/// Pool of connected MCP servers, keyed by the YAML name. Owns the long-lived
/// rmcp client/service handles. Built once at startup; cloned via `Arc` to any
/// crate that needs to hand MCP tools to an LLM agent.
pub struct McpServers {
    servers: HashMap<String, McpServer>,
}

struct McpServer {
    _service: RunningService<RoleClient, ()>,
    sink: ServerSink,
    tools: HashMap<String, rmcp::model::Tool>,
}

impl McpServers {
    /// Connect to every server in `configs` and list their tools. Each
    /// connection error fails fast so the operator finds out at boot, not
    /// on the first request.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn connect(configs: HashMap<String, McpServerConfig>) -> Result<Self, McpError> {
        let mut servers = HashMap::with_capacity(configs.len());
        for (name, cfg) in configs {
            let server = McpServer::connect(&name, cfg).await?;
            servers.insert(name, server);
        }
        Ok(Self { servers })
    }

    /// Build the rig-shaped tool list for one agent's `mcp_tools:` section.
    /// `agent` is the agent's name (used only in error messages so config
    /// pointers remain readable).
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub fn tools_for(
        &self,
        agent: &str,
        accesses: &[McpToolAccess],
    ) -> Result<Vec<Box<dyn ToolDyn>>, McpError> {
        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
        for access in accesses {
            let server =
                self.servers
                    .get(&access.server)
                    .ok_or_else(|| McpError::ServerNotConfigured {
                        agent: agent.to_string(),
                        server: access.server.clone(),
                    })?;
            let picked: Vec<_> = match &access.only {
                Some(names) => names
                    .iter()
                    .map(|name| {
                        server
                            .tools
                            .get(name)
                            .cloned()
                            .ok_or_else(|| McpError::ToolNotFound {
                                agent: agent.to_string(),
                                server: access.server.clone(),
                                tool: name.clone(),
                            })
                    })
                    .collect::<Result<_, _>>()?,
                None => server.tools.values().cloned().collect(),
            };
            for tool in picked {
                tools.push(Box::new(McpTool::from_mcp_server(
                    tool,
                    server.sink.clone(),
                )));
            }
        }
        Ok(tools)
    }
}

impl McpServer {
    async fn connect(name: &str, config: McpServerConfig) -> Result<Self, McpError> {
        let service = match config {
            McpServerConfig::Http { url } => {
                let transport = StreamableHttpClientTransport::from_uri(url);
                ().serve(transport)
                    .await
                    .map_err(|source| McpError::Connect {
                        server: name.to_string(),
                        source: Box::new(source),
                    })?
            }
            McpServerConfig::Stdio { args, command, env } => {
                let mut cmd = Command::new(&command);
                cmd.args(&args);
                if !env.is_empty() {
                    cmd.envs(&env);
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
