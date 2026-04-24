use std::collections::HashMap;
use std::pin::Pin;

use futures::stream::{Stream, StreamExt};
use rig::agent::{MultiTurnStreamItem, PromptRequest};
use rig::client::CompletionClient;
use rig::completion::{CompletionModel, GetTokenUsage, Message as RigMessage, PromptError};
use rig::providers::{anthropic, cohere, deepseek, gemini, groq, openai};
use rig::streaming::{StreamedAssistantContent, StreamingPrompt};
use rig::tool::ToolDyn;
use rig::tool::rmcp::McpTool;
use rmcp::ServiceExt;
use rmcp::service::{RoleClient, RunningService, ServerSink};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use tokio::process::Command;

use crate::{AgentConfig, Completion, Config, McpServerConfig, PrompterError, ProviderKind, Usage};

const MAX_TURNS: usize = 8;

pub struct RigPrompter {
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
/// token usage. Tool-call internals are intentionally not surfaced — they
/// happen inside `rig`'s multi-turn loop and the OpenAI client never sees them.
#[derive(Clone, Debug)]
pub enum StreamEvent {
    Delta(String),
    Done { usage: Usage },
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
            agents: config.agents,
            backends,
            mcp_servers,
        })
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

    fn find_agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.name == name)
    }
}

impl Prompter for RigPrompter {
    fn agents(&self) -> &[AgentConfig] {
        &self.agents
    }

    async fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
    ) -> Result<Completion, PrompterError> {
        let agent = self
            .find_agent(agent_name)
            .ok_or_else(|| PrompterError::UnknownAgent(agent_name.to_string()))?;
        let backend = self.backends.get(&agent.provider).ok_or_else(|| {
            PrompterError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let attachments = self.collect_mcp_attachments(agent)?;
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        let result = match backend {
            Backend::Anthropic(c) => conversation.send(c, &agent.model, attachments).await,
            Backend::Cohere(c) => conversation.send(c, &agent.model, attachments).await,
            Backend::Deepseek(c) => conversation.send(c, &agent.model, attachments).await,
            Backend::Gemini(c) => conversation.send(c, &agent.model, attachments).await,
            Backend::Groq(c) => conversation.send(c, &agent.model, attachments).await,
            Backend::Openai(c) => conversation.send(c, &agent.model, attachments).await,
        };
        result.map_err(PrompterError::from)
    }

    async fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
    ) -> Result<CompletionStream, PrompterError> {
        let agent = self
            .find_agent(agent_name)
            .ok_or_else(|| PrompterError::UnknownAgent(agent_name.to_string()))?;
        let backend = self.backends.get(&agent.provider).ok_or_else(|| {
            PrompterError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let attachments = self.collect_mcp_attachments(agent)?;
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        match backend {
            Backend::Anthropic(c) => conversation.stream(c, &agent.model, attachments).await,
            Backend::Cohere(c) => conversation.stream(c, &agent.model, attachments).await,
            Backend::Deepseek(c) => conversation.stream(c, &agent.model, attachments).await,
            Backend::Gemini(c) => conversation.stream(c, &agent.model, attachments).await,
            Backend::Groq(c) => conversation.stream(c, &agent.model, attachments).await,
            Backend::Openai(c) => conversation.stream(c, &agent.model, attachments).await,
        }
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
    ) -> Result<Completion, PromptError>
    where
        C: CompletionClient,
        C::CompletionModel: 'static,
    {
        let mut builder = client.agent(model);
        if !self.preamble.is_empty() {
            builder = builder.preamble(&self.preamble);
        }
        let mcp_tools: Vec<Box<dyn ToolDyn>> = mcp
            .into_iter()
            .flat_map(|attach| {
                let sink = attach.sink;
                attach.tools.into_iter().map(move |t| {
                    Box::new(McpTool::from_mcp_server(t, sink.clone())) as Box<dyn ToolDyn>
                })
            })
            .collect();
        let agent = if mcp_tools.is_empty() {
            builder.build()
        } else {
            builder.tools(mcp_tools).build()
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
        let mcp_tools: Vec<Box<dyn ToolDyn>> = mcp
            .into_iter()
            .flat_map(|attach| {
                let sink = attach.sink;
                attach.tools.into_iter().map(move |t| {
                    Box::new(McpTool::from_mcp_server(t, sink.clone())) as Box<dyn ToolDyn>
                })
            })
            .collect();
        let agent = if mcp_tools.is_empty() {
            builder.build()
        } else {
            builder.tools(mcp_tools).build()
        };
        let inner = agent
            .stream_prompt(self.prompt)
            .with_history(self.history)
            .multi_turn(MAX_TURNS)
            .await;
        let mapped = inner.filter_map(|item| async move {
            match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(t))) => {
                    Some(Ok(StreamEvent::Delta(t.text)))
                }
                Ok(MultiTurnStreamItem::FinalResponse(fr)) => Some(Ok(StreamEvent::Done {
                    usage: fr.usage().into(),
                })),
                Ok(_) => None,
                Err(e) => Some(Err(PrompterError::Streaming(e.to_string()))),
            }
        });
        Ok(Box::pin(mapped))
    }
}
