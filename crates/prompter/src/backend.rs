use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

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
use tokio::process::Command;

use crate::{AgentConfig, Completion, Config, McpServerConfig, PrompterError, ProviderKind, Usage};

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

struct McpAttachment {
    sink: ServerSink,
    tools: Vec<rmcp::model::Tool>,
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
/// dispatch so callers (the admin UI, observability sinks) can record what
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

/// Abstraction over an LLM backend. Implementations answer completion
/// requests — either as a single response or as a stream of incremental
/// events. The server talks to this trait so tests can drive the HTTP
/// handler with a scripted implementation instead of a real provider.
pub trait Prompter: Send + Sync {
    fn agents(&self) -> &[AgentConfig];

    fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
    ) -> impl std::future::Future<Output = Result<Completion, PrompterError>> + Send;

    fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
    ) -> impl std::future::Future<Output = Result<CompletionStream, PrompterError>> + Send;

    /// One-off prompt bypassing agent-config lookup. No MCP tools, no
    /// preamble merging — just `provider`, `model`, the supplied preamble
    /// and messages. Used for internal tasks like memory fact extraction.
    fn prompt_with(
        &self,
        provider: ProviderKind,
        model: &str,
        preamble: &str,
        messages: Vec<Message>,
    ) -> impl std::future::Future<Output = Result<Completion, PrompterError>> + Send;
}

impl RigPrompter {
    pub async fn new(config: Config) -> Result<Self, PrompterError> {
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
    ) -> Result<Completion, PrompterError> {
        RigPrompterInner::complete_with_depth(&self.inner, agent_name, messages, 0).await
    }

    async fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
    ) -> Result<CompletionStream, PrompterError> {
        RigPrompterInner::complete_streaming_with_depth(&self.inner, agent_name, messages, 0).await
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

    fn collect_mcp_attachments(
        &self,
        agent: &AgentConfig,
    ) -> Result<Vec<McpAttachment>, PrompterError> {
        let mut attachments = Vec::with_capacity(agent.mcp_tools.len());
        for access in &agent.mcp_tools {
            let server = self.mcp_servers.get(&access.server).ok_or_else(|| {
                PrompterError::McpServerNotConfigured {
                    agent: agent.name.clone(),
                    server: access.server.clone(),
                }
            })?;
            let tools = match &access.only {
                Some(names) => {
                    let mut picked = Vec::with_capacity(names.len());
                    for name in names {
                        let tool = server.tools.get(name).ok_or_else(|| {
                            PrompterError::McpToolNotFound {
                                agent: agent.name.clone(),
                                server: access.server.clone(),
                                tool: name.clone(),
                            }
                        })?;
                        picked.push(tool.clone());
                    }
                    picked
                }
                None => server.tools.values().cloned().collect(),
            };
            attachments.push(McpAttachment {
                sink: server.sink.clone(),
                tools,
            });
        }
        Ok(attachments)
    }

    /// Build one `SubagentTool` per entry in `agent.subagents`. The `depth`
    /// parameter is the current agent's depth; each tool it hands out
    /// captures that depth and invokes the target at `depth + 1`.
    fn collect_subagent_tools(
        self: &Arc<Self>,
        agent: &AgentConfig,
        depth: usize,
    ) -> Vec<Box<dyn ToolDyn>> {
        agent
            .subagents
            .iter()
            .map(|sub_name| {
                let target = self
                    .find_agent(sub_name)
                    .expect("subagent existence is validated at config load");
                let purpose = target
                    .purpose
                    .clone()
                    .unwrap_or_else(|| format!("Invoke the '{}' subagent.", target.name));
                Box::new(SubagentTool {
                    inner: Arc::clone(self),
                    target_name: target.name.clone(),
                    purpose,
                    depth,
                }) as Box<dyn ToolDyn>
            })
            .collect()
    }

    async fn complete_with_depth(
        self: &Arc<Self>,
        agent_name: &str,
        messages: Vec<Message>,
        depth: usize,
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
        let mcp_attachments = self.collect_mcp_attachments(agent)?;
        let subagent_tools = self.collect_subagent_tools(agent, depth);
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        let result = match backend {
            Backend::Anthropic(c) => {
                conversation
                    .send(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Cohere(c) => {
                conversation
                    .send(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Deepseek(c) => {
                conversation
                    .send(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Gemini(c) => {
                conversation
                    .send(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Groq(c) => {
                conversation
                    .send(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Openai(c) => {
                conversation
                    .send(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
        };
        result.map_err(PrompterError::from)
    }

    async fn complete_streaming_with_depth(
        self: &Arc<Self>,
        agent_name: &str,
        messages: Vec<Message>,
        depth: usize,
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
        let mcp_attachments = self.collect_mcp_attachments(agent)?;
        let subagent_tools = self.collect_subagent_tools(agent, depth);
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        match backend {
            Backend::Anthropic(c) => {
                conversation
                    .stream(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Cohere(c) => {
                conversation
                    .stream(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Deepseek(c) => {
                conversation
                    .stream(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Gemini(c) => {
                conversation
                    .stream(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Groq(c) => {
                conversation
                    .stream(c, &agent.model, mcp_attachments, subagent_tools)
                    .await
            }
            Backend::Openai(c) => {
                conversation
                    .stream(c, &agent.model, mcp_attachments, subagent_tools)
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
            Backend::Anthropic(c) => conversation.send(c, model, vec![], vec![]).await,
            Backend::Cohere(c) => conversation.send(c, model, vec![], vec![]).await,
            Backend::Deepseek(c) => conversation.send(c, model, vec![], vec![]).await,
            Backend::Gemini(c) => conversation.send(c, model, vec![], vec![]).await,
            Backend::Groq(c) => conversation.send(c, model, vec![], vec![]).await,
            Backend::Openai(c) => conversation.send(c, model, vec![], vec![]).await,
        };
        result.map_err(PrompterError::from)
    }
}

/// A rig tool that, when called, invokes another agent as a fresh
/// conversation. The subagent runs under its own preamble, its own MCP tool
/// list, and its own bounded tool loop; the final assistant text becomes
/// this tool's return value. Hop depth is captured at construction so
/// pathological A→B→A→B chains are bounded by `MAX_SUBAGENT_DEPTH`.
struct SubagentTool {
    inner: Arc<RigPrompterInner>,
    target_name: String,
    purpose: String,
    depth: usize,
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
        let inner = Arc::clone(&self.inner);
        let target = self.target_name.clone();
        let depth = self.depth;
        Box::pin(async move {
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
            match RigPrompterInner::complete_with_depth(&inner, &target, messages, next_depth).await
            {
                Ok(completion) => Ok(completion.text),
                Err(err) => Err(ToolError::ToolCallError(Box::new(err))),
            }
        })
    }
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
        mcp: Vec<McpAttachment>,
        subagent_tools: Vec<Box<dyn ToolDyn>>,
    ) -> Result<Completion, PromptError>
    where
        C: CompletionClient,
        C::CompletionModel: 'static,
    {
        let mut builder = client.agent(model);
        if !self.preamble.is_empty() {
            builder = builder.preamble(&self.preamble);
        }
        let mut tools: Vec<Box<dyn ToolDyn>> = mcp
            .into_iter()
            .flat_map(|attach| {
                let sink = attach.sink;
                attach.tools.into_iter().map(move |t| {
                    Box::new(McpTool::from_mcp_server(t, sink.clone())) as Box<dyn ToolDyn>
                })
            })
            .collect();
        tools.extend(subagent_tools);
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
        mcp: Vec<McpAttachment>,
        subagent_tools: Vec<Box<dyn ToolDyn>>,
    ) -> Result<CompletionStream, PrompterError>
    where
        C: CompletionClient,
        C::CompletionModel: 'static,
        <C::CompletionModel as CompletionModel>::StreamingResponse: GetTokenUsage,
    {
        use std::collections::HashSet;

        let mut builder = client.agent(model);
        if !self.preamble.is_empty() {
            builder = builder.preamble(&self.preamble);
        }
        let mut tools: Vec<Box<dyn ToolDyn>> = mcp
            .into_iter()
            .flat_map(|attach| {
                let sink = attach.sink;
                attach.tools.into_iter().map(move |t| {
                    Box::new(McpTool::from_mcp_server(t, sink.clone())) as Box<dyn ToolDyn>
                })
            })
            .collect();
        // Snapshot subagent names before handing the boxes to rig — we need
        // them inside the stream map to classify each ToolCall event as
        // either MCP or Subagent.
        let subagent_names: Arc<HashSet<String>> =
            Arc::new(subagent_tools.iter().map(|t| t.name()).collect());
        tools.extend(subagent_tools);
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
/// are rendered as a stable `"<image>"` placeholder so the admin UI at least
/// shows that an image was returned. Lossy on purpose — the admin view is for
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
