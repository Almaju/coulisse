use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{Stream, StreamExt};
use rig::agent::{MultiTurnStreamItem, PromptRequest};
use rig::client::CompletionClient;
use rig::completion::{
    CompletionModel, GetTokenUsage, Message as RigMessage, PromptError, ToolDefinition,
};
use rig::providers::{anthropic, cohere, deepseek, gemini, groq, openai};
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt};
use rig::tool::rmcp::McpTool;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use rmcp::ServiceExt;
use rmcp::service::{RoleClient, RunningService, ServerSink};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde_json::json;
use telemetry::{Ctx, Event, EventId, EventKind, Sink as TelemetrySink};
use tokio::process::Command;

use config::{AgentConfig, Config, McpServerConfig, ProviderKind};

use crate::{Completion, PrompterError, Usage};

const MAX_TURNS: usize = 8;
/// How many nested subagent calls are allowed before the hop limit kicks in.
/// A→B→A→… is cut off once the depth reaches this number. Four levels is
/// deep enough for realistic orchestrator → specialist → sub-specialist
/// patterns without letting pathological loops burn tokens.
const MAX_SUBAGENT_DEPTH: usize = 4;

pub struct RigPrompter {
    inner: Arc<RigPrompterInner>,
}

struct RigPrompterInner {
    agents: Vec<AgentConfig>,
    backends: HashMap<ProviderKind, Backend>,
    mcp_servers: HashMap<String, McpServer>,
    /// Optional observability sink. When `Some`, every tool invocation
    /// (MCP or subagent, at any depth) is recorded as a `ToolCall` event.
    /// Kept off the hot path by short-circuiting when `None`, so tests and
    /// internal prompter callers that don't care about telemetry pay no cost.
    telemetry: Option<Arc<TelemetrySink>>,
}

enum Backend {
    Anthropic(anthropic::Client),
    Cohere(cohere::Client),
    Deepseek(deepseek::Client),
    Gemini(gemini::Client),
    Groq(groq::Client),
    Openai(openai::Client),
}

struct McpServer {
    _service: RunningService<RoleClient, ()>,
    sink: ServerSink,
    tools: HashMap<String, rmcp::model::Tool>,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub content: String,
    pub role: Role,
}

#[derive(Clone, Copy, Debug)]
pub enum Role {
    Assistant,
    System,
    User,
}

/// One event in a streamed completion. `Delta` carries an incremental piece of
/// the assistant's response; `Done` is yielded at the end with cumulative
/// token usage. `ToolCall` and `ToolResult` expose rig's multi-turn tool
/// dispatch so callers (the studio UI, observability sinks) can record what
/// the agent tried and what came back. Correlate the pair by `call_id`.
#[derive(Clone, Debug)]
pub enum StreamEvent {
    Delta(String),
    Done {
        usage: Usage,
    },
    ToolCall {
        args: String,
        call_id: String,
        kind: ToolCallKind,
        tool_name: String,
    },
    ToolResult {
        call_id: String,
        error: Option<String>,
        result: Option<String>,
    },
}

/// How a tool invocation was serviced — an MCP server, or another agent
/// exposed as a tool (subagent).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallKind {
    Mcp,
    Subagent,
}

pub type CompletionStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, PrompterError>> + Send>>;

/// Result of `RigPrompterInner::build_tools`: the full `ToolDyn` list to
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
pub trait Prompter: Send + Sync {
    fn agents(&self) -> &[AgentConfig];

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
    ) -> impl std::future::Future<Output = Result<Completion, PrompterError>> + Send;

    fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        ctx: Ctx,
    ) -> impl std::future::Future<Output = Result<CompletionStream, PrompterError>> + Send;

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
    ) -> impl std::future::Future<Output = Result<Completion, PrompterError>> + Send;
}

impl RigPrompter {
    /// Build a prompter from `config`, optionally wired to a telemetry
    /// sink. When `telemetry` is `Some`, every tool invocation at any depth
    /// (MCP or subagent) is recorded as a `ToolCall` event so the studio UI
    /// can reconstruct nested subagent trees. Tests that don't care pass
    /// `None` and pay no observability cost.
    pub async fn new(
        config: Config,
        telemetry: Option<Arc<TelemetrySink>>,
    ) -> Result<Self, PrompterError> {
        let mut backends = HashMap::with_capacity(config.providers.len());
        for (kind, provider) in config.providers {
            let backend = match kind {
                ProviderKind::Anthropic => {
                    Backend::Anthropic(anthropic::Client::new(&provider.api_key).map_err(
                        |source| PrompterError::ClientInit {
                            provider: kind,
                            source,
                        },
                    )?)
                }
                ProviderKind::Cohere => {
                    Backend::Cohere(cohere::Client::new(&provider.api_key).map_err(|source| {
                        PrompterError::ClientInit {
                            provider: kind,
                            source,
                        }
                    })?)
                }
                ProviderKind::Deepseek => {
                    Backend::Deepseek(deepseek::Client::new(&provider.api_key).map_err(
                        |source| PrompterError::ClientInit {
                            provider: kind,
                            source,
                        },
                    )?)
                }
                ProviderKind::Gemini => {
                    Backend::Gemini(gemini::Client::new(&provider.api_key).map_err(|source| {
                        PrompterError::ClientInit {
                            provider: kind,
                            source,
                        }
                    })?)
                }
                ProviderKind::Groq => {
                    Backend::Groq(groq::Client::new(&provider.api_key).map_err(|source| {
                        PrompterError::ClientInit {
                            provider: kind,
                            source,
                        }
                    })?)
                }
                ProviderKind::Openai => {
                    Backend::Openai(openai::Client::new(&provider.api_key).map_err(|source| {
                        PrompterError::ClientInit {
                            provider: kind,
                            source,
                        }
                    })?)
                }
            };
            backends.insert(kind, backend);
        }

        let mut mcp_servers = HashMap::with_capacity(config.mcp.len());
        for (name, cfg) in config.mcp {
            let server = McpServer::connect(&name, cfg).await?;
            mcp_servers.insert(name, server);
        }

        Ok(Self {
            inner: Arc::new(RigPrompterInner {
                agents: config.agents,
                backends,
                mcp_servers,
                telemetry,
            }),
        })
    }
}

impl Prompter for RigPrompter {
    fn agents(&self) -> &[AgentConfig] {
        &self.inner.agents
    }

    async fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        ctx: Ctx,
    ) -> Result<Completion, PrompterError> {
        RigPrompterInner::complete_with_depth(&self.inner, agent_name, messages, 0, ctx).await
    }

    async fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        ctx: Ctx,
    ) -> Result<CompletionStream, PrompterError> {
        RigPrompterInner::complete_streaming_with_depth(&self.inner, agent_name, messages, 0, ctx)
            .await
    }

    async fn prompt_with(
        &self,
        provider: ProviderKind,
        model: &str,
        preamble: &str,
        messages: Vec<Message>,
    ) -> Result<Completion, PrompterError> {
        self.inner
            .prompt_with(provider, model, preamble, messages)
            .await
    }
}

impl RigPrompterInner {
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
    ) -> Result<BuiltTools, PrompterError> {
        use std::collections::HashSet;

        let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
        for access in &agent.mcp_tools {
            let server = self.mcp_servers.get(&access.server).ok_or_else(|| {
                PrompterError::McpServerNotConfigured {
                    agent: agent.name.clone(),
                    server: access.server.clone(),
                }
            })?;
            let picked: Vec<_> = match &access.only {
                Some(names) => names
                    .iter()
                    .map(|name| {
                        server.tools.get(name).cloned().ok_or_else(|| {
                            PrompterError::McpToolNotFound {
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
            let target = self
                .find_agent(sub_name)
                .expect("subagent existence is validated at config load");
            let purpose = target
                .purpose
                .clone()
                .unwrap_or_else(|| format!("Invoke the '{}' subagent.", target.name));
            tools.push(Box::new(SubagentTool {
                ctx,
                depth,
                inner: Arc::clone(self),
                purpose,
                sink: self.telemetry.clone(),
                target_name: target.name.clone(),
            }));
        }
        Ok((tools, subagent_names))
    }

    async fn complete_with_depth(
        self: &Arc<Self>,
        agent_name: &str,
        messages: Vec<Message>,
        depth: usize,
        ctx: Ctx,
    ) -> Result<Completion, PrompterError> {
        if depth > MAX_SUBAGENT_DEPTH {
            return Err(PrompterError::SubagentDepthExceeded {
                limit: MAX_SUBAGENT_DEPTH,
                subagent: agent_name.to_string(),
            });
        }
        let agent = self
            .find_agent(agent_name)
            .ok_or_else(|| PrompterError::UnknownAgent(agent_name.to_string()))?;
        let backend = self.backends.get(&agent.provider).ok_or_else(|| {
            PrompterError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let (tools, _) = self.build_tools(agent, depth, ctx)?;
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        let result = match backend {
            Backend::Anthropic(c) => conversation.send(c, &agent.model, tools).await,
            Backend::Cohere(c) => conversation.send(c, &agent.model, tools).await,
            Backend::Deepseek(c) => conversation.send(c, &agent.model, tools).await,
            Backend::Gemini(c) => conversation.send(c, &agent.model, tools).await,
            Backend::Groq(c) => conversation.send(c, &agent.model, tools).await,
            Backend::Openai(c) => conversation.send(c, &agent.model, tools).await,
        };
        result.map_err(PrompterError::from)
    }

    async fn complete_streaming_with_depth(
        self: &Arc<Self>,
        agent_name: &str,
        messages: Vec<Message>,
        depth: usize,
        ctx: Ctx,
    ) -> Result<CompletionStream, PrompterError> {
        if depth > MAX_SUBAGENT_DEPTH {
            return Err(PrompterError::SubagentDepthExceeded {
                limit: MAX_SUBAGENT_DEPTH,
                subagent: agent_name.to_string(),
            });
        }
        let agent = self
            .find_agent(agent_name)
            .ok_or_else(|| PrompterError::UnknownAgent(agent_name.to_string()))?;
        let backend = self.backends.get(&agent.provider).ok_or_else(|| {
            PrompterError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let (tools, subagent_names) = self.build_tools(agent, depth, ctx)?;
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        match backend {
            Backend::Anthropic(c) => {
                conversation
                    .stream(c, &agent.model, tools, subagent_names)
                    .await
            }
            Backend::Cohere(c) => {
                conversation
                    .stream(c, &agent.model, tools, subagent_names)
                    .await
            }
            Backend::Deepseek(c) => {
                conversation
                    .stream(c, &agent.model, tools, subagent_names)
                    .await
            }
            Backend::Gemini(c) => {
                conversation
                    .stream(c, &agent.model, tools, subagent_names)
                    .await
            }
            Backend::Groq(c) => {
                conversation
                    .stream(c, &agent.model, tools, subagent_names)
                    .await
            }
            Backend::Openai(c) => {
                conversation
                    .stream(c, &agent.model, tools, subagent_names)
                    .await
            }
        }
    }

    async fn prompt_with(
        &self,
        provider: ProviderKind,
        model: &str,
        preamble: &str,
        messages: Vec<Message>,
    ) -> Result<Completion, PrompterError> {
        let backend = self
            .backends
            .get(&provider)
            .ok_or(PrompterError::ProviderNotConfigured {
                agent: "<internal>".into(),
                provider,
            })?;
        let conversation = Conversation::from_messages(messages, preamble)?;
        let result = match backend {
            Backend::Anthropic(c) => conversation.send(c, model, vec![]).await,
            Backend::Cohere(c) => conversation.send(c, model, vec![]).await,
            Backend::Deepseek(c) => conversation.send(c, model, vec![]).await,
            Backend::Gemini(c) => conversation.send(c, model, vec![]).await,
            Backend::Groq(c) => conversation.send(c, model, vec![]).await,
            Backend::Openai(c) => conversation.send(c, model, vec![]).await,
        };
        result.map_err(PrompterError::from)
    }
}

/// A rig tool that, when called, invokes another agent as a fresh
/// conversation. The subagent runs under its own preamble, its own MCP tool
/// list, and its own bounded tool loop; the final assistant text becomes
/// this tool's return value. Hop depth is captured at construction so
/// pathological A→B→A→B chains are bounded by `MAX_SUBAGENT_DEPTH`.
///
/// When a telemetry `sink` is configured, this tool emits its own
/// `ToolCall` event and passes a child context down so the subagent's
/// inner tool calls nest under this one in the studio tree.
struct SubagentTool {
    ctx: Ctx,
    depth: usize,
    inner: Arc<RigPrompterInner>,
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
            let outcome = RigPrompterInner::complete_with_depth(
                &inner, &target, messages, next_depth, child_ctx,
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
    async fn connect(name: &str, config: McpServerConfig) -> Result<Self, PrompterError> {
        let service = match config {
            McpServerConfig::Http { url } => {
                let transport = StreamableHttpClientTransport::from_uri(url);
                ().serve(transport)
                    .await
                    .map_err(|source| PrompterError::McpConnect {
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
                    TokioChildProcess::new(cmd).map_err(|source| PrompterError::SpawnMcp {
                        server: name.to_string(),
                        source,
                    })?;
                ().serve(transport)
                    .await
                    .map_err(|source| PrompterError::McpConnect {
                        server: name.to_string(),
                        source: Box::new(source),
                    })?
            }
        };
        let listed = service
            .list_tools(Default::default())
            .await
            .map_err(|source| PrompterError::McpListTools {
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

struct Conversation {
    history: Vec<RigMessage>,
    preamble: String,
    prompt: RigMessage,
}

impl Conversation {
    fn from_messages(messages: Vec<Message>, agent_preamble: &str) -> Result<Self, PrompterError> {
        let mut preamble_parts = Vec::new();
        if !agent_preamble.is_empty() {
            preamble_parts.push(agent_preamble.to_string());
        }
        let mut turns: Vec<RigMessage> = Vec::new();
        for m in messages {
            match m.role {
                Role::Assistant => turns.push(RigMessage::assistant(m.content)),
                Role::System => {
                    if !m.content.is_empty() {
                        preamble_parts.push(m.content);
                    }
                }
                Role::User => turns.push(RigMessage::user(m.content)),
            }
        }
        let prompt = turns.pop().ok_or(PrompterError::EmptyConversation)?;
        Ok(Self {
            history: turns,
            preamble: preamble_parts.join("\n\n"),
            prompt,
        })
    }

    async fn send<C>(
        self,
        client: &C,
        model: &str,
        tools: Vec<Box<dyn ToolDyn>>,
    ) -> Result<Completion, PromptError>
    where
        C: CompletionClient,
        C::CompletionModel: 'static,
    {
        let mut builder = client.agent(model);
        if !self.preamble.is_empty() {
            builder = builder.preamble(&self.preamble);
        }
        let agent = if tools.is_empty() {
            builder.build()
        } else {
            builder.tools(tools).build()
        };
        let response = PromptRequest::from_agent(&agent, self.prompt)
            .with_history(self.history)
            .max_turns(MAX_TURNS)
            .extended_details()
            .await?;
        Ok(Completion {
            text: response.output,
            usage: response.usage.into(),
        })
    }

    async fn stream<C>(
        self,
        client: &C,
        model: &str,
        tools: Vec<Box<dyn ToolDyn>>,
        subagent_names: Arc<std::collections::HashSet<String>>,
    ) -> Result<CompletionStream, PrompterError>
    where
        C: CompletionClient,
        C::CompletionModel: 'static,
        <C::CompletionModel as CompletionModel>::StreamingResponse: GetTokenUsage,
    {
        let mut builder = client.agent(model);
        if !self.preamble.is_empty() {
            builder = builder.preamble(&self.preamble);
        }
        let agent = if tools.is_empty() {
            builder.build()
        } else {
            builder.tools(tools).build()
        };
        let inner = agent
            .stream_prompt(self.prompt)
            .with_history(self.history)
            .multi_turn(MAX_TURNS)
            .await;
        let mapped = inner.filter_map(move |item| {
            let subagent_names = Arc::clone(&subagent_names);
            async move {
                match item {
                    Ok(MultiTurnStreamItem::StreamAssistantItem(
                        StreamedAssistantContent::Text(t),
                    )) => Some(Ok(StreamEvent::Delta(t.text))),
                    Ok(MultiTurnStreamItem::StreamAssistantItem(
                        StreamedAssistantContent::ToolCall {
                            tool_call,
                            internal_call_id,
                        },
                    )) => {
                        let tool_name = tool_call.function.name.clone();
                        let kind = if subagent_names.contains(&tool_name) {
                            ToolCallKind::Subagent
                        } else {
                            ToolCallKind::Mcp
                        };
                        let args = tool_call.function.arguments.to_string();
                        Some(Ok(StreamEvent::ToolCall {
                            args,
                            call_id: internal_call_id,
                            kind,
                            tool_name,
                        }))
                    }
                    Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                        tool_result,
                        internal_call_id,
                    })) => {
                        let result = flatten_tool_result(&tool_result);
                        Some(Ok(StreamEvent::ToolResult {
                            call_id: internal_call_id,
                            error: None,
                            result: Some(result),
                        }))
                    }
                    Ok(MultiTurnStreamItem::FinalResponse(fr)) => Some(Ok(StreamEvent::Done {
                        usage: fr.usage().into(),
                    })),
                    Ok(_) => None,
                    Err(e) => Some(Err(PrompterError::Streaming(e.to_string()))),
                }
            }
        });
        Ok(Box::pin(mapped))
    }
}

/// Collapse rig's `ToolResult.content` (a `OneOrMany<ToolResultContent>`) into
/// a single plain-text string for persistence. Text parts are joined; images
/// are rendered as a stable `"<image>"` placeholder so the studio UI at least
/// shows that an image was returned. Lossy on purpose — the studio view is for
/// human debugging, not verbatim replay.
fn flatten_tool_result(tool_result: &rig::completion::message::ToolResult) -> String {
    use rig::completion::message::ToolResultContent;
    tool_result
        .content
        .iter()
        .map(|part| match part {
            ToolResultContent::Text(t) => t.text.clone(),
            ToolResultContent::Image(_) => "<image>".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
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
