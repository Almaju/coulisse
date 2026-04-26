use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;

use coulisse_core::{AgentResolver, OneShotError, OneShotPrompt, UserId};
use mcp::McpServers;
use providers::{
    Completion, CompletionStream, Conversation, Message, ProviderKind, Providers, Role,
    ToolCallKind,
};
use rig::tool::ToolDyn;

use crate::AgentsError;
use crate::config::AgentConfig;
use crate::tools::{SubagentTool, TelemetryTool};

/// How many nested subagent calls are allowed before the hop limit kicks in.
/// A→B→A→… is cut off once the depth reaches this number. Four levels is
/// deep enough for realistic orchestrator → specialist → sub-specialist
/// patterns without letting pathological loops burn tokens.
const MAX_SUBAGENT_DEPTH: usize = 4;

pub struct RigAgents {
    inner: Arc<AgentsInner>,
}

/// Shared inner state held behind an `Arc` so subagent tools (which call
/// back into the runtime) can clone the handle cheaply. `pub(crate)` so
/// the tools module can read `resolver` and call `complete_with_depth`
/// on the subagent re-entry path.
pub(crate) struct AgentsInner {
    agents: Vec<AgentConfig>,
    mcp: Arc<McpServers>,
    providers: Providers,
    /// Maps subagent names to concrete agent names at call time. The
    /// runtime never sees `experiments` directly — it just asks the
    /// resolver. Cli wires the impl (currently `ExperimentResolver`).
    pub(crate) resolver: Arc<dyn AgentResolver>,
}

/// Result of `AgentsInner::build_tools`: the full `ToolDyn` list to hand
/// to rig, plus a snapshot of subagent names so the streaming classifier
/// can tag outgoing tool events as Subagent vs Mcp. Aliased to dodge the
/// `clippy::type_complexity` lint on the return type.
type BuiltTools = (Vec<Box<dyn ToolDyn>>, Arc<HashSet<String>>);

/// Abstraction over the multi-agent runtime. Implementations answer
/// completion requests — either as a single response or as a stream of
/// incremental events. The cli's chat handler talks to this trait so tests
/// can drive it with a scripted implementation instead of a real provider.
///
/// Tool invocations and subagent calls are observed via the `tracing`
/// crate: callers run the future inside a `turn` span carrying `user_id`
/// and `turn_id`, and child `tool_call` spans nest automatically. The
/// telemetry crate's SqliteLayer mirrors those spans to the studio.
pub trait Agents: Send + Sync {
    fn agents(&self) -> &[AgentConfig];

    /// Run the named agent and return its final reply. The agent name is
    /// the already-resolved variant; callers (cli's chat handler) do
    /// experiment resolution before reaching this method. The caller is
    /// expected to drive this future inside a `turn` tracing span so any
    /// nested `tool_call` spans inherit the correlation ids. `user_id`
    /// drives sticky-by-user variant routing on subagent calls —
    /// observability is the tracing subscriber's job, but variant
    /// resolution is a real domain dependency that must be plumbed.
    fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        user_id: UserId,
    ) -> impl std::future::Future<Output = Result<Completion, AgentsError>> + Send;

    fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        user_id: UserId,
    ) -> impl std::future::Future<Output = Result<CompletionStream, AgentsError>> + Send;

    /// One-off prompt bypassing agent-config lookup. No MCP tools, no
    /// preamble merging — just `provider`, `model`, the supplied preamble
    /// and messages. Used for internal tasks like memory fact extraction.
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
    pub mcp: Arc<McpServers>,
    pub providers: HashMap<ProviderKind, providers::ProviderConfig>,
    /// Resolves subagent names (which may be experiment names) to concrete
    /// agent names at call time. Cli builds this from
    /// `experiments::ExperimentResolver`.
    pub resolver: Arc<dyn AgentResolver>,
}

impl RigAgents {
    /// Build agents from the YAML slices declared under `agents:` and
    /// `providers:`, plus a pre-connected `McpServers` pool and a
    /// subagent name resolver. Tool invocations are recorded as
    /// `tool_call` tracing spans on every MCP and subagent call regardless
    /// of depth — observability is the subscriber's job, not this crate's.
    pub fn new(config: BootConfig) -> Result<Self, AgentsError> {
        let providers = Providers::new(config.providers).map_err(AgentsError::from)?;
        Ok(Self {
            inner: Arc::new(AgentsInner {
                agents: config.agents,
                mcp: config.mcp,
                providers,
                resolver: config.resolver,
            }),
        })
    }
}

impl Agents for RigAgents {
    fn agents(&self) -> &[AgentConfig] {
        &self.inner.agents
    }

    async fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        user_id: UserId,
    ) -> Result<Completion, AgentsError> {
        AgentsInner::complete_with_depth(&self.inner, agent_name, messages, 0, user_id).await
    }

    async fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        user_id: UserId,
    ) -> Result<CompletionStream, AgentsError> {
        AgentsInner::complete_streaming_with_depth(&self.inner, agent_name, messages, 0, user_id)
            .await
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
        user_id: UserId,
    ) -> Result<BuiltTools, AgentsError> {
        let raw_mcp_tools = self.mcp.tools_for(&agent.name, &agent.mcp_tools)?;
        let mut tools: Vec<Box<dyn ToolDyn>> = raw_mcp_tools
            .into_iter()
            .map(|inner| -> Box<dyn ToolDyn> {
                Box::new(TelemetryTool {
                    inner,
                    kind: ToolCallKind::Mcp,
                })
            })
            .collect();

        let subagent_names: Arc<HashSet<String>> =
            Arc::new(agent.subagents.iter().cloned().collect());
        for sub_name in &agent.subagents {
            let purpose = self.subagent_purpose(sub_name);
            tools.push(Box::new(SubagentTool {
                depth,
                inner: Arc::clone(self),
                purpose,
                target_name: sub_name.clone(),
                user_id,
            }));
        }
        Ok((tools, subagent_names))
    }

    /// Tool description for a subagent reference. Subagent names share
    /// the agent + experiment namespace: agents looks at its own table
    /// first, then defers to the resolver for experiment purposes.
    /// Validation already guarantees the name exists somewhere.
    fn subagent_purpose(&self, name: &str) -> String {
        if let Some(agent) = self.find_agent(name) {
            return agent
                .purpose
                .clone()
                .unwrap_or_else(|| format!("Invoke the '{}' subagent.", agent.name));
        }
        self.resolver
            .purpose(name)
            .unwrap_or_else(|| format!("Invoke the '{name}' subagent."))
    }

    pub(crate) async fn complete_with_depth(
        self: &Arc<Self>,
        agent_name: &str,
        messages: Vec<Message>,
        depth: usize,
        user_id: UserId,
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
        let provider = self.providers.get(agent.provider).ok_or_else(|| {
            AgentsError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let (tools, _) = self.build_tools(agent, depth, user_id)?;
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        provider
            .send(conversation, &agent.model, tools)
            .await
            .map_err(AgentsError::from)
    }

    async fn complete_streaming_with_depth(
        self: &Arc<Self>,
        agent_name: &str,
        messages: Vec<Message>,
        depth: usize,
        user_id: UserId,
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
        let provider = self.providers.get(agent.provider).ok_or_else(|| {
            AgentsError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let (tools, subagent_names) = self.build_tools(agent, depth, user_id)?;
        let conversation = Conversation::from_messages(messages, &agent.preamble)?;
        provider
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
        let provider = self
            .providers
            .get(provider)
            .ok_or(AgentsError::ProviderNotConfigured {
                agent: "<internal>".into(),
                provider,
            })?;
        let conversation = Conversation::from_messages(messages, preamble)?;
        provider
            .send(conversation, model, vec![])
            .await
            .map_err(AgentsError::from)
    }
}
