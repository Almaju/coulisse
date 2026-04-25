use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use backends::{
    Backends, Completion, CompletionStream, Conversation, Message, ProviderKind, Role, ToolCallKind,
};
use coulisse_core::{OneShotError, OneShotPrompt};
use experiments::ExperimentRouter;
use memory::Store;
use rig::completion::ToolDefinition;
use rig::tool::rmcp::McpTool;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use rmcp::ServiceExt;
use rmcp::service::{RoleClient, RunningService, ServerSink};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde_json::json;
use telemetry::{Ctx, Event, EventId, EventKind, Sink as TelemetrySink};
use tokio::process::Command;

use crate::AgentsError;
use crate::config::{AgentConfig, McpServerConfig};

/// How many nested subagent calls are allowed before the hop limit kicks in.
/// A→B→A→… is cut off once the depth reaches this number. Four levels is
/// deep enough for realistic orchestrator → specialist → sub-specialist
/// patterns without letting pathological loops burn tokens.
const MAX_SUBAGENT_DEPTH: usize = 4;

pub struct RigAgents {
    inner: Arc<AgentsInner>,
}

struct AgentsInner {
    agents: Vec<AgentConfig>,
    backends: Backends,
    mcp_servers: HashMap<String, McpServer>,
    /// A/B routing table. Populated from `config.experiments` at startup;
    /// resolves an addressable name (agent or experiment) to a concrete
    /// agent at request time. Empty when no experiments are configured —
    /// `resolve` then short-circuits to passthrough.
    router: ExperimentRouter,
    /// Optional handle to the score store. Required for bandit-strategy
    /// subagent calls (which need to read recent mean scores at call
    /// time). When `None`, bandit subagents fall back to forced
    /// exploration — fine for tests and small deployments.
    score_store: Option<Arc<Store>>,
    /// Optional observability sink. When `Some`, every tool invocation
    /// (MCP or subagent, at any depth) is recorded as a `ToolCall` event.
    /// Kept off the hot path by short-circuiting when `None`, so tests and
    /// internal prompter callers that don't care about telemetry pay no cost.
    telemetry: Option<Arc<TelemetrySink>>,
}

struct McpServer {
    _service: RunningService<RoleClient, ()>,
    sink: ServerSink,
    tools: HashMap<String, rmcp::model::Tool>,
}

/// Result of `AgentsInner::build_tools`: the full `ToolDyn` list to
/// hand to rig, plus a snapshot of subagent names so the streaming
/// classifier can tag outgoing tool events as Subagent vs Mcp. Aliased to
/// dodge the `clippy::type_complexity` lint on the return type.
type BuiltTools = (
    Vec<Box<dyn ToolDyn>>,
    Arc<std::collections::HashSet<String>>,
);

/// Abstraction over an LLM backend. Implementations answer completion
/// requests — either as a single response or as a stream of incremental
/// events. The server talks to this trait so tests can drive the HTTP
/// handler with a scripted implementation instead of a real provider.
pub trait Agents: Send + Sync {
    fn agents(&self) -> &[AgentConfig];

    /// A/B routing table for this prompter. The proxy consults this
    /// before dispatching so that experiment names addressable as
    /// `model` resolve to a concrete variant per request.
    fn router(&self) -> &ExperimentRouter;

    /// Run the named agent and return its final reply. `ctx` carries the
    /// per-request correlation id plus the parent `EventId` under which
    /// this completion's tool invocations should nest. The `parent` field
    /// typically points at the caller's `TurnStart` event for top-level
    /// requests, or at a `ToolCall` event when a subagent recurses.
    fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        ctx: Ctx,
    ) -> impl std::future::Future<Output = Result<Completion, AgentsError>> + Send;

    fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        ctx: Ctx,
    ) -> impl std::future::Future<Output = Result<CompletionStream, AgentsError>> + Send;

    /// One-off prompt bypassing agent-config lookup. No MCP tools, no
    /// preamble merging — just `provider`, `model`, the supplied preamble
    /// and messages. Used for internal tasks like memory fact extraction.
    /// Not tied to user-facing telemetry, hence no `ctx`.
    fn prompt_with(
        &self,
        provider: ProviderKind,
        model: &str,
        preamble: &str,
        messages: Vec<Message>,
    ) -> impl std::future::Future<Output = Result<Completion, AgentsError>> + Send;
}

/// Bundled inputs for `RigAgents::new`. Grouped into one struct so cli
/// can hand each YAML slice to the right field without a long argument
/// list, and so adding a new optional input doesn't break every call site.
pub struct BootConfig {
    pub agents: Vec<AgentConfig>,
    pub experiments: Vec<experiments::ExperimentConfig>,
    pub mcp: HashMap<String, McpServerConfig>,
    pub providers: HashMap<ProviderKind, backends::ProviderConfig>,
}

impl RigAgents {
    /// Build agents from the YAML slices declared under `agents:`,
    /// `experiments:`, `mcp:`, and `providers:`. When `telemetry` is
    /// `Some`, every tool invocation at any depth (MCP or subagent) is
    /// recorded as a `ToolCall` event so the studio UI can reconstruct
    /// nested subagent trees. Tests that don't care pass `None` and pay
    /// no observability cost.
    pub async fn new(
        config: BootConfig,
        telemetry: Option<Arc<TelemetrySink>>,
        score_store: Option<Arc<Store>>,
    ) -> Result<Self, AgentsError> {
        let backends = Backends::new(config.providers).map_err(AgentsError::from)?;

        let mut mcp_servers = HashMap::with_capacity(config.mcp.len());
        for (name, cfg) in config.mcp {
            let server = McpServer::connect(&name, cfg).await?;
            mcp_servers.insert(name, server);
        }

        let router = ExperimentRouter::new(config.experiments);
        Ok(Self {
            inner: Arc::new(AgentsInner {
                agents: config.agents,
                backends,
                mcp_servers,
                router,
                score_store,
                telemetry,
            }),
        })
    }
}

impl Agents for RigAgents {
    fn agents(&self) -> &[AgentConfig] {
        &self.inner.agents
    }

    fn router(&self) -> &ExperimentRouter {
        &self.inner.router
    }

    async fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        ctx: Ctx,
    ) -> Result<Completion, AgentsError> {
        AgentsInner::complete_with_depth(&self.inner, agent_name, messages, 0, ctx).await
    }

    async fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        ctx: Ctx,
    ) -> Result<CompletionStream, AgentsError> {
        AgentsInner::complete_streaming_with_depth(&self.inner, agent_name, messages, 0, ctx).await
    }

    async fn prompt_with(
        &self,
        provider: ProviderKind,
        model: &str,
        preamble: &str,
        messages: Vec<Message>,
    ) -> Result<Completion, AgentsError> {
        self.inner
            .prompt_with(provider, model, preamble, messages)
            .await
    }
}

impl OneShotPrompt for RigAgents {
    fn one_shot<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        preamble: &'a str,
        user_text: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, OneShotError>> + Send + 'a>> {
        Box::pin(async move {
            let provider_kind = ProviderKind::parse(provider)
                .ok_or_else(|| OneShotError::new(format!("unknown provider '{provider}'")))?;
            let messages = vec![Message {
                content: user_text.to_string(),
                role: Role::User,
            }];
            self.inner
                .prompt_with(provider_kind, model, preamble, messages)
                .await
                .map(|c| c.text)
                .map_err(|e| OneShotError::new(e.to_string()))
        })
    }
}

impl AgentsInner {
    fn find_agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// Build the full tool list the agent will see: MCP tools (wrapped in
    /// a `TelemetryTool` recorder) followed by subagent tools (which
    /// record themselves). Also returns a snapshot of subagent names so the
    /// streaming classifier in `Conversation::stream` can label outgoing
    /// `ToolCall` events as `Subagent` vs `Mcp`.
    fn build_tools(
        self: &Arc<Self>,
        agent: &AgentConfig,
        depth: usize,
        ctx: Ctx,
    ) -> Result<BuiltTools, AgentsError> {
        use std::collections::HashSet;

        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
        for access in &agent.mcp_tools {
            let server = self.mcp_servers.get(&access.server).ok_or_else(|| {
                AgentsError::McpServerNotConfigured {
                    agent: agent.name.clone(),
                    server: access.server.clone(),
                }
            })?;
            let picked: Vec<_> = match &access.only {
                Some(names) => names
                    .iter()
                    .map(|name| {
                        server.tools.get(name).cloned().ok_or_else(|| {
                            AgentsError::McpToolNotFound {
                                agent: agent.name.clone(),
                                server: access.server.clone(),
                                tool: name.clone(),
                            }
                        })
                    })
                    .collect::<Result<_, _>>()?,
                None => server.tools.values().cloned().collect(),
            };
            for tool in picked {
                let raw: Box<dyn ToolDyn> =
                    Box::new(McpTool::from_mcp_server(tool, server.sink.clone()));
                tools.push(Box::new(TelemetryTool {
                    ctx,
                    inner: raw,
                    kind: ToolCallKind::Mcp,
                    sink: self.telemetry.clone(),
                }));
            }
        }

        let subagent_names: Arc<HashSet<String>> =
            Arc::new(agent.subagents.iter().cloned().collect());
        for sub_name in &agent.subagents {
            let purpose = self.subagent_purpose(sub_name);
            tools.push(Box::new(SubagentTool {
                ctx,
                depth,
                inner: Arc::clone(self),
                purpose,
                sink: self.telemetry.clone(),
                target_name: sub_name.clone(),
            }));
        }
        Ok((tools, subagent_names))
    }

    /// Tool description for a subagent reference. Subagent names share
    /// the agent + experiment namespace, so look in both — the agent
    /// table first since the names cannot collide. Validation already
    /// guarantees the name exists somewhere.
    fn subagent_purpose(&self, name: &str) -> String {
        if let Some(agent) = self.find_agent(name) {
            return agent
                .purpose
                .clone()
                .unwrap_or_else(|| format!("Invoke the '{}' subagent.", agent.name));
        }
        if let Some(experiment) = self.router.get(name) {
            return experiment
                .purpose
                .clone()
                .unwrap_or_else(|| format!("Invoke the '{}' subagent.", experiment.name));
        }
        unreachable!("subagent name validated at config load: {name}")
    }

    async fn complete_with_depth(
        self: &Arc<Self>,
        agent_name: &str,
        messages: Vec<Message>,
        depth: usize,
        ctx: Ctx,
    ) -> Result<Completion, AgentsError> {
        if depth > MAX_SUBAGENT_DEPTH {
            return Err(AgentsError::SubagentDepthExceeded {
                limit: MAX_SUBAGENT_DEPTH,
                subagent: agent_name.to_string(),
            });
        }
        let agent = self
            .find_agent(agent_name)
            .ok_or_else(|| AgentsError::UnknownAgent(agent_name.to_string()))?;
        let backend = self.backends.get(agent.provider).ok_or_else(|| {
            AgentsError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let (tools, _) = self.build_tools(agent, depth, ctx)?;
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        backend
            .send(conversation, &agent.model, tools)
            .await
            .map_err(AgentsError::from)
    }

    async fn complete_streaming_with_depth(
        self: &Arc<Self>,
        agent_name: &str,
        messages: Vec<Message>,
        depth: usize,
        ctx: Ctx,
    ) -> Result<CompletionStream, AgentsError> {
        if depth > MAX_SUBAGENT_DEPTH {
            return Err(AgentsError::SubagentDepthExceeded {
                limit: MAX_SUBAGENT_DEPTH,
                subagent: agent_name.to_string(),
            });
        }
        let agent = self
            .find_agent(agent_name)
            .ok_or_else(|| AgentsError::UnknownAgent(agent_name.to_string()))?;
        let backend = self.backends.get(agent.provider).ok_or_else(|| {
            AgentsError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let (tools, subagent_names) = self.build_tools(agent, depth, ctx)?;
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        backend
            .stream(conversation, &agent.model, tools, subagent_names)
            .await
            .map_err(AgentsError::from)
    }

    async fn prompt_with(
        &self,
        provider: ProviderKind,
        model: &str,
        preamble: &str,
        messages: Vec<Message>,
    ) -> Result<Completion, AgentsError> {
        let backend = self
            .backends
            .get(provider)
            .ok_or(AgentsError::ProviderNotConfigured {
                agent: "<internal>".into(),
                provider,
            })?;
        let conversation = Conversation::from_messages(messages, preamble)?;
        backend
            .send(conversation, model, vec![])
            .await
            .map_err(AgentsError::from)
    }
}

/// A rig tool that, when called, invokes another agent as a fresh
/// conversation. The subagent runs under its own preamble, its own MCP tool
/// list, and its own bounded tool loop; the final assistant text becomes
/// this tool's return value. Hop depth is captured at construction so
/// pathological A→B→A→B chains are bounded by `MAX_SUBAGENT_DEPTH`.
///
/// `target_name` is the addressable name as written in YAML — either an
/// agent or an experiment. Resolution happens at call time so each
/// subagent invocation goes through the router, picking a variant for
/// the calling user.
///
/// When a telemetry `sink` is configured, this tool emits its own
/// `ToolCall` event and passes a child context down so the subagent's
/// inner tool calls nest under this one in the studio tree.
struct SubagentTool {
    ctx: Ctx,
    depth: usize,
    inner: Arc<AgentsInner>,
    purpose: String,
    sink: Option<Arc<TelemetrySink>>,
    target_name: String,
}

impl ToolDyn for SubagentTool {
    fn name(&self) -> String {
        self.target_name.clone()
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        let name = self.target_name.clone();
        let description = self.purpose.clone();
        Box::pin(async move {
            ToolDefinition {
                name,
                description,
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Natural-language message or instruction to send to the subagent. The subagent starts with a fresh context and sees only this message.",
                        }
                    },
                    "required": ["message"],
                }),
            }
        })
    }

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let ctx = self.ctx;
        let depth = self.depth;
        let inner = Arc::clone(&self.inner);
        let sink = self.sink.clone();
        let target = self.target_name.clone();
        Box::pin(async move {
            let event_id = EventId::new();
            let child_ctx = ctx.child(event_id);
            let started = Instant::now();

            let parsed: serde_json::Value =
                serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            let message = parsed
                .get("message")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ToolError::ToolCallError(Box::<dyn std::error::Error + Send + Sync>::from(
                        "subagent tool call is missing required 'message' field",
                    ))
                })?
                .to_string();
            let messages = vec![Message {
                role: Role::User,
                content: message,
            }];
            let next_depth = depth.saturating_add(1);
            // Subagent name may be an experiment — resolve per-user so
            // the variant is picked at call time, consistent with the
            // sticky-by-user hashing the proxy applies at the top level.
            // Bandit experiments additionally consult recent mean
            // scores; without a score store wired in, that lookup
            // returns no data and the bandit falls back to forced
            // exploration.
            let scores = if let (Some(store), Some((judge, criterion, since))) = (
                inner.score_store.as_ref(),
                inner.router.bandit_query(&target),
            ) {
                store
                    .mean_scores_by_agent(&judge, &criterion, since)
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let resolved = inner
                .router
                .resolve_with_scores(&target, ctx.user_id, &scores);
            let agent_name = resolved.agent.into_owned();
            let outcome = AgentsInner::complete_with_depth(
                &inner,
                &agent_name,
                messages,
                next_depth,
                child_ctx,
            )
            .await;
            let duration_ms = started.elapsed().as_millis() as u64;

            if let Some(sink) = sink {
                let payload = tool_call_payload(
                    &target,
                    ToolCallKind::Subagent,
                    &args,
                    outcome.as_ref().map(|c| c.text.as_str()).ok(),
                    outcome.as_ref().err().map(|e| e.to_string()),
                );
                let event = Event::new(
                    ctx.correlation_id,
                    ctx.user_id,
                    ctx.parent,
                    EventKind::ToolCall,
                    payload,
                )
                .with_id(event_id)
                .with_duration_ms(duration_ms);
                if let Err(err) = sink.emit(event).await {
                    eprintln!("telemetry emit failed for subagent tool call: {err}");
                }
            }

            match outcome {
                Ok(completion) => Ok(completion.text),
                Err(err) => Err(ToolError::ToolCallError(Box::new(err))),
            }
        })
    }
}

/// Tool decorator that records a `ToolCall` event around any inner
/// `ToolDyn`. Used to instrument MCP tools so the studio UI can see what
/// arguments were sent and what came back, including errors — the exact
/// blind spot that let hallucinated "database error" replies hide real
/// upstream failures.
struct TelemetryTool {
    ctx: Ctx,
    inner: Box<dyn ToolDyn>,
    kind: ToolCallKind,
    sink: Option<Arc<TelemetrySink>>,
}

impl ToolDyn for TelemetryTool {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn definition(&self, prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        self.inner.definition(prompt)
    }

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let ctx = self.ctx;
        let kind = self.kind;
        let name = self.inner.name();
        let sink = self.sink.clone();
        let inner_call = self.inner.call(args.clone());
        Box::pin(async move {
            let started = Instant::now();
            let result = inner_call.await;
            let duration_ms = started.elapsed().as_millis() as u64;

            if let Some(sink) = sink {
                let payload = tool_call_payload(
                    &name,
                    kind,
                    &args,
                    result.as_ref().ok().map(|s| s.as_str()),
                    result.as_ref().err().map(|e| e.to_string()),
                );
                let event = Event::new(
                    ctx.correlation_id,
                    ctx.user_id,
                    ctx.parent,
                    EventKind::ToolCall,
                    payload,
                )
                .with_duration_ms(duration_ms);
                if let Err(err) = sink.emit(event).await {
                    eprintln!("telemetry emit failed for {name}: {err}");
                }
            }
            result
        })
    }
}

fn tool_call_payload(
    tool_name: &str,
    kind: ToolCallKind,
    args: &str,
    result: Option<&str>,
    error: Option<String>,
) -> serde_json::Value {
    json!({
        "args": args,
        "error": error,
        "kind": match kind {
            ToolCallKind::Mcp => "mcp",
            ToolCallKind::Subagent => "subagent",
        },
        "result": result,
        "tool_name": tool_name,
    })
}

impl McpServer {
    async fn connect(name: &str, config: McpServerConfig) -> Result<Self, AgentsError> {
        let service = match config {
            McpServerConfig::Http { url } => {
                let transport = StreamableHttpClientTransport::from_uri(url);
                ().serve(transport)
                    .await
                    .map_err(|source| AgentsError::McpConnect {
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
                let transport =
                    TokioChildProcess::new(cmd).map_err(|source| AgentsError::SpawnMcp {
                        server: name.to_string(),
                        source,
                    })?;
                ().serve(transport)
                    .await
                    .map_err(|source| AgentsError::McpConnect {
                        server: name.to_string(),
                        source: Box::new(source),
                    })?
            }
        };
        let listed = service
            .list_tools(Default::default())
            .await
            .map_err(|source| AgentsError::McpListTools {
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
mod telemetry_tool_tests {
    use super::*;
    use memory::UserId;
    use rig::completion::ToolDefinition;
    use sqlx::SqlitePool;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;
    use telemetry::{Ctx, EventKind, Sink, TurnId};

    /// Minimal `ToolDyn` that returns whatever result/error the test wires
    /// in. Replaces `McpTool` in tests so the wrapper logic can be exercised
    /// without a live MCP server.
    struct FakeTool {
        name: String,
        outcome: Result<String, String>,
    }

    impl ToolDyn for FakeTool {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
            let name = self.name.clone();
            Box::pin(async move {
                ToolDefinition {
                    name,
                    description: "fake".into(),
                    parameters: json!({}),
                }
            })
        }

        fn call(&self, _args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
            let outcome = self.outcome.clone();
            Box::pin(async move {
                outcome.map_err(|e| {
                    ToolError::ToolCallError(Box::<dyn std::error::Error + Send + Sync>::from(e))
                })
            })
        }
    }

    async fn fresh_sink() -> Arc<Sink> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePool::connect_with(options).await.unwrap();
        Arc::new(Sink::open(pool).await.unwrap())
    }

    #[tokio::test]
    async fn wrapper_emits_event_on_success() {
        let sink = fresh_sink().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let ctx = Ctx::new(user, turn);

        let wrapper = TelemetryTool {
            ctx,
            inner: Box::new(FakeTool {
                name: "search_jobs".into(),
                outcome: Ok("hello".into()),
            }),
            kind: ToolCallKind::Mcp,
            sink: Some(Arc::clone(&sink)),
        };
        let out = wrapper.call("{\"q\":\"x\"}".into()).await.unwrap();
        assert_eq!(out, "hello");

        let events = sink.fetch_turn(user, turn).await.unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind, EventKind::ToolCall);
        assert_eq!(e.payload["tool_name"], "search_jobs");
        assert_eq!(e.payload["kind"], "mcp");
        assert_eq!(e.payload["result"], "hello");
        assert!(e.payload["error"].is_null());
        assert!(e.duration_ms.is_some());
    }

    #[tokio::test]
    async fn wrapper_captures_tool_error() {
        let sink = fresh_sink().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let ctx = Ctx::new(user, turn);

        let wrapper = TelemetryTool {
            ctx,
            inner: Box::new(FakeTool {
                name: "search_jobs".into(),
                outcome: Err("column j.search_vector does not exist".into()),
            }),
            kind: ToolCallKind::Mcp,
            sink: Some(Arc::clone(&sink)),
        };
        let err = wrapper.call("{}".into()).await.unwrap_err();
        // rig's ToolError renders to the underlying Display; assert that the
        // error text flows through unchanged.
        assert!(err.to_string().contains("search_vector"));

        let events = sink.fetch_turn(user, turn).await.unwrap();
        assert_eq!(events.len(), 1);
        let payload = &events[0].payload;
        assert_eq!(payload["kind"], "mcp");
        assert!(payload["result"].is_null());
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or("")
                .contains("search_vector"),
            "expected error text in payload, got {}",
            payload["error"]
        );
    }

    #[tokio::test]
    async fn wrapper_without_sink_is_transparent() {
        let wrapper = TelemetryTool {
            ctx: Ctx::synthetic(),
            inner: Box::new(FakeTool {
                name: "search_jobs".into(),
                outcome: Ok("ok".into()),
            }),
            kind: ToolCallKind::Mcp,
            sink: None,
        };
        let out = wrapper.call("{}".into()).await.unwrap();
        assert_eq!(out, "ok");
    }
}
