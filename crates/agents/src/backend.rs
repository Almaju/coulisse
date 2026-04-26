use std::pin::Pin;
use std::sync::Arc;

use coulisse_core::{OneShotError, OneShotPrompt, ScoreLookup, UserId};
use experiments::ExperimentRouter;
use mcp::McpServers;
use providers::{
    Completion, CompletionStream, Conversation, Message, ProviderKind, Providers, Role,
    ToolCallKind,
};
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use serde_json::json;
use tracing::{Instrument, info_span};

use crate::AgentsError;
use crate::config::AgentConfig;

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
    mcp: Arc<McpServers>,
    providers: Providers,
    /// A/B routing table. Populated from `config.experiments` at startup;
    /// resolves an addressable name (agent or experiment) to a concrete
    /// agent at request time. Empty when no experiments are configured —
    /// `resolve` then short-circuits to passthrough.
    router: ExperimentRouter,
    /// Optional handle to a score reader. Required for bandit-strategy
    /// subagent calls (which need to read recent mean scores at call
    /// time). When `None`, bandit subagents fall back to forced
    /// exploration — fine for tests and small deployments.
    scores: Option<Arc<dyn ScoreLookup>>,
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
///
/// Tool invocations and subagent calls are observed via the `tracing`
/// crate: callers run the future inside a `turn` span carrying `user_id`
/// and `turn_id`, and child `tool_call` spans nest automatically. The
/// telemetry crate's SqliteLayer mirrors those spans to the studio.
pub trait Agents: Send + Sync {
    fn agents(&self) -> &[AgentConfig];

    /// A/B routing table for this prompter. The proxy consults this
    /// before dispatching so that experiment names addressable as
    /// `model` resolve to a concrete variant per request.
    fn router(&self) -> &ExperimentRouter;

    /// Run the named agent and return its final reply. The caller is
    /// expected to drive this future inside a `turn` tracing span so any
    /// nested `tool_call` spans inherit the correlation ids.
    /// `user_id` drives sticky-by-user variant routing on subagent calls
    /// — observability is the tracing subscriber's job, but variant
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
    pub experiments: Vec<experiments::ExperimentConfig>,
    pub mcp: Arc<McpServers>,
    pub providers: std::collections::HashMap<ProviderKind, providers::ProviderConfig>,
}

impl RigAgents {
    /// Build agents from the YAML slices declared under `agents:`,
    /// `experiments:`, and `providers:`, plus a pre-connected
    /// `McpServers` pool. Tool invocations are recorded as `tool_call`
    /// tracing spans on every MCP and subagent call regardless of depth —
    /// observability is the subscriber's job, not this crate's.
    pub fn new(
        config: BootConfig,
        scores: Option<Arc<dyn ScoreLookup>>,
    ) -> Result<Self, AgentsError> {
        let providers = Providers::new(config.providers).map_err(AgentsError::from)?;
        let router = ExperimentRouter::new(config.experiments);
        Ok(Self {
            inner: Arc::new(AgentsInner {
                agents: config.agents,
                mcp: config.mcp,
                providers,
                router,
                scores,
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
        use std::collections::HashSet;

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
        let backend = self.providers.get(agent.provider).ok_or_else(|| {
            AgentsError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let (tools, _) = self.build_tools(agent, depth, user_id)?;
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
        let backend = self.providers.get(agent.provider).ok_or_else(|| {
            AgentsError::ProviderNotConfigured {
                agent: agent.name.clone(),
                provider: agent.provider,
            }
        })?;
        let (tools, subagent_names) = self.build_tools(agent, depth, user_id)?;
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
            .providers
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
/// Each invocation opens a `tool_call` tracing span so the subagent's
/// inner tool calls nest underneath it in the studio tree.
struct SubagentTool {
    depth: usize,
    inner: Arc<AgentsInner>,
    purpose: String,
    target_name: String,
    /// Calling user — only used for sticky-by-user variant resolution
    /// when the subagent target is an experiment. Not observability.
    user_id: UserId,
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
        let depth = self.depth;
        let inner = Arc::clone(&self.inner);
        let target = self.target_name.clone();
        let user_id = self.user_id;
        let span = info_span!(
            "tool_call",
            args = %args,
            error = tracing::field::Empty,
            kind = "subagent",
            result = tracing::field::Empty,
            tool_name = %target,
        );
        Box::pin(
            async move {
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
                let scores = if let (Some(scores), Some((judge, criterion, since))) =
                    (inner.scores.as_ref(), inner.router.bandit_query(&target))
                {
                    scores
                        .mean_scores_by_agent(&judge, &criterion, since)
                        .await
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let resolved = inner.router.resolve_with_scores(&target, user_id, &scores);
                let agent_name = resolved.agent.into_owned();
                let outcome = AgentsInner::complete_with_depth(
                    &inner,
                    &agent_name,
                    messages,
                    next_depth,
                    user_id,
                )
                .await;

                let span = tracing::Span::current();
                match &outcome {
                    Ok(completion) => span.record("result", completion.text.as_str()),
                    Err(err) => span.record("error", err.to_string().as_str()),
                };

                match outcome {
                    Ok(completion) => Ok(completion.text),
                    Err(err) => Err(ToolError::ToolCallError(Box::new(err))),
                }
            }
            .instrument(span),
        )
    }
}

/// Tool decorator that opens a `tool_call` tracing span around any inner
/// `ToolDyn`. The telemetry crate's SqliteLayer mirrors the span (with
/// `args`, `result`, `error`, `tool_name`, `kind` fields) into the
/// `events` and `tool_calls` tables — closing the blind spot that let
/// hallucinated tool-failure replies hide real upstream errors.
struct TelemetryTool {
    inner: Box<dyn ToolDyn>,
    kind: ToolCallKind,
}

impl ToolDyn for TelemetryTool {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn definition(&self, prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        self.inner.definition(prompt)
    }

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let kind_str = match self.kind {
            ToolCallKind::Mcp => "mcp",
            ToolCallKind::Subagent => "subagent",
        };
        let name = self.inner.name();
        let inner_call = self.inner.call(args.clone());
        let span = info_span!(
            "tool_call",
            args = %args,
            error = tracing::field::Empty,
            kind = kind_str,
            result = tracing::field::Empty,
            tool_name = %name,
        );
        Box::pin(
            async move {
                let result = inner_call.await;
                let span = tracing::Span::current();
                match &result {
                    Ok(text) => span.record("result", text.as_str()),
                    Err(err) => span.record("error", err.to_string().as_str()),
                };
                result
            }
            .instrument(span),
        )
    }
}

#[cfg(test)]
mod telemetry_tool_tests {
    use super::*;
    use coulisse_core::UserId;
    use rig::completion::ToolDefinition;
    use sqlx::SqlitePool;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;
    use telemetry::{Sink, SqliteLayer, TurnId};
    use tracing::Instrument;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

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

    async fn fresh_pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePool::connect_with(options).await.unwrap();
        Sink::open(pool.clone()).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn wrapper_emits_event_on_success() {
        let pool = fresh_pool().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let (layer, guard) = SqliteLayer::spawn(pool.clone());
        let _default = tracing_subscriber::registry().with(layer).set_default();

        async {
            let wrapper = TelemetryTool {
                inner: Box::new(FakeTool {
                    name: "search_jobs".into(),
                    outcome: Ok("hello".into()),
                }),
                kind: ToolCallKind::Mcp,
            };
            let out = wrapper.call("{\"q\":\"x\"}".into()).await.unwrap();
            assert_eq!(out, "hello");
        }
        .instrument(tracing::info_span!(
            "turn",
            agent = "test",
            turn_id = %turn.0,
            user_id = %user.0,
            user_message = "",
        ))
        .await;

        guard.flush().await;
        let sink = Sink::open(pool).await.unwrap();
        let events = sink.fetch_turn(user, turn).await.unwrap();
        let tool_evt = events
            .iter()
            .find(|e| e.kind == telemetry::EventKind::ToolCall)
            .expect("tool_call event recorded");
        assert_eq!(tool_evt.payload["tool_name"], "search_jobs");
        assert_eq!(tool_evt.payload["kind"], "mcp");
        assert_eq!(tool_evt.payload["result"], "hello");
        assert!(
            tool_evt
                .payload
                .get("error")
                .map(|v| v.is_null())
                .unwrap_or(true)
        );
        assert!(tool_evt.duration_ms.is_some());
    }

    #[tokio::test]
    async fn wrapper_captures_tool_error() {
        let pool = fresh_pool().await;
        let user = UserId::new();
        let turn = TurnId::new();
        let (layer, guard) = SqliteLayer::spawn(pool.clone());
        let _default = tracing_subscriber::registry().with(layer).set_default();

        let err = async {
            let wrapper = TelemetryTool {
                inner: Box::new(FakeTool {
                    name: "search_jobs".into(),
                    outcome: Err("column j.search_vector does not exist".into()),
                }),
                kind: ToolCallKind::Mcp,
            };
            wrapper.call("{}".into()).await.unwrap_err()
        }
        .instrument(tracing::info_span!(
            "turn",
            agent = "test",
            turn_id = %turn.0,
            user_id = %user.0,
            user_message = "",
        ))
        .await;
        // rig's ToolError renders to the underlying Display; assert that the
        // error text flows through unchanged.
        assert!(err.to_string().contains("search_vector"));

        guard.flush().await;
        let sink = Sink::open(pool).await.unwrap();
        let events = sink.fetch_turn(user, turn).await.unwrap();
        let tool_evt = events
            .iter()
            .find(|e| e.kind == telemetry::EventKind::ToolCall)
            .expect("tool_call event recorded");
        let payload = &tool_evt.payload;
        assert_eq!(payload["kind"], "mcp");
        assert!(payload.get("result").map(|v| v.is_null()).unwrap_or(true));
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
    async fn wrapper_is_transparent_without_subscriber() {
        // No subscriber installed — span emissions are no-ops; the tool
        // still runs and returns its underlying result.
        let wrapper = TelemetryTool {
            inner: Box::new(FakeTool {
                name: "search_jobs".into(),
                outcome: Ok("ok".into()),
            }),
            kind: ToolCallKind::Mcp,
        };
        let out = wrapper.call("{}".into()).await.unwrap();
        assert_eq!(out, "ok");
    }
}
