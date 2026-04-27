//! Provider-agnostic conversation dispatch.
//!
//! `Conversation` packs a turn's messages into the shape rig wants
//! (history + final prompt + preamble). `Provider::send` and
//! `Provider::stream` then dispatch to the configured provider's Rig
//! client without exposing the variant set to callers — this is the
//! whole reason the `Provider` enum exists.

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use futures::stream::{Stream, StreamExt};
use rig::agent::{MultiTurnStreamItem, PromptRequest};
use rig::client::CompletionClient;
use rig::completion::{CompletionModel, GetTokenUsage, Message as RigMessage, PromptError};
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt};
use rig::tool::ToolDyn;
use serde::Serialize;
use thiserror::Error;

use crate::Provider;

/// Hard cap on how many tool-calling rounds rig will run within a single
/// completion. Eight rounds is enough for realistic multi-step tool use
/// without inviting runaway loops on misbehaving prompts.
pub const MAX_TURNS: usize = 8;

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

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Usage {
    pub cache_creation_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl From<rig::completion::Usage> for Usage {
    fn from(u: rig::completion::Usage) -> Self {
        Self {
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cached_input_tokens: u.cached_input_tokens,
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub text: String,
    pub usage: Usage,
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

pub use coulisse_core::ToolCallKind;

pub type CompletionStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, CallError>> + Send>>;

/// Errors raised while dispatching a single turn to a provider. Higher-
/// level orchestration errors (unknown agent, subagent depth, etc.) live
/// in `agents::AgentsError`; this one stops at the provider boundary.
#[derive(Debug, Error)]
pub enum CallError {
    #[error("conversation has no user or assistant messages")]
    EmptyConversation,
    #[error("provider request failed: {0}")]
    Provider(#[from] PromptError),
    #[error("provider streaming failed: {0}")]
    Streaming(String),
}

/// History + preamble + final prompt, ready to hand to a Rig agent.
/// Build with `from_messages`, then dispatch via `Provider::send` or
/// `Provider::stream`.
pub struct Conversation {
    history: Vec<RigMessage>,
    preamble: String,
    prompt: RigMessage,
}

impl Conversation {
    /// Pack chat-style messages into rig's expected shape: every
    /// `System` message merges into the preamble; the last non-system
    /// message becomes the prompt; everything else becomes history.
    /// `agent_preamble` is prepended to any system messages so the
    /// agent's static instructions always lead.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub fn from_messages(messages: Vec<Message>, agent_preamble: &str) -> Result<Self, CallError> {
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
        let prompt = turns.pop().ok_or(CallError::EmptyConversation)?;
        Ok(Self {
            history: turns,
            preamble: preamble_parts.join("\n\n"),
            prompt,
        })
    }
}

impl Provider {
    /// Run the conversation synchronously and return the final reply.
    /// Dispatches to the matching Rig client internally — callers never
    /// need to match on `Provider` variants.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn send(
        &self,
        conversation: Conversation,
        model: &str,
        tools: Vec<Box<dyn ToolDyn>>,
    ) -> Result<Completion, CallError> {
        match self {
            Provider::Anthropic(c) => send_with(c, conversation, model, tools).await,
            Provider::Cohere(c) => send_with(c, conversation, model, tools).await,
            Provider::Deepseek(c) => send_with(c, conversation, model, tools).await,
            Provider::Gemini(c) => send_with(c, conversation, model, tools).await,
            Provider::Groq(c) => send_with(c, conversation, model, tools).await,
            Provider::Openai(c) => send_with(c, conversation, model, tools).await,
        }
    }

    /// Stream the conversation. Each `StreamEvent::ToolCall` is tagged
    /// `Subagent` if its tool name appears in `subagent_names`,
    /// otherwise `Mcp` — the classification lives here because it's
    /// trivial to do on the fly and saves callers a wrapping pass.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn stream(
        &self,
        conversation: Conversation,
        model: &str,
        tools: Vec<Box<dyn ToolDyn>>,
        subagent_names: Arc<HashSet<String>>,
    ) -> Result<CompletionStream, CallError> {
        match self {
            Provider::Anthropic(c) => {
                stream_with(c, conversation, model, tools, subagent_names).await
            }
            Provider::Cohere(c) => stream_with(c, conversation, model, tools, subagent_names).await,
            Provider::Deepseek(c) => {
                stream_with(c, conversation, model, tools, subagent_names).await
            }
            Provider::Gemini(c) => stream_with(c, conversation, model, tools, subagent_names).await,
            Provider::Groq(c) => stream_with(c, conversation, model, tools, subagent_names).await,
            Provider::Openai(c) => stream_with(c, conversation, model, tools, subagent_names).await,
        }
    }
}

async fn send_with<C>(
    client: &C,
    conversation: Conversation,
    model: &str,
    tools: Vec<Box<dyn ToolDyn>>,
) -> Result<Completion, CallError>
where
    C: CompletionClient,
    C::CompletionModel: 'static,
{
    let mut builder = client.agent(model);
    if !conversation.preamble.is_empty() {
        builder = builder.preamble(&conversation.preamble);
    }
    let agent = if tools.is_empty() {
        builder.build()
    } else {
        builder.tools(tools).build()
    };
    let response = PromptRequest::from_agent(&agent, conversation.prompt)
        .with_history(conversation.history)
        .max_turns(MAX_TURNS)
        .extended_details()
        .await?;
    Ok(Completion {
        text: response.output,
        usage: response.usage.into(),
    })
}

async fn stream_with<C>(
    client: &C,
    conversation: Conversation,
    model: &str,
    tools: Vec<Box<dyn ToolDyn>>,
    subagent_names: Arc<HashSet<String>>,
) -> Result<CompletionStream, CallError>
where
    C: CompletionClient,
    C::CompletionModel: 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse: GetTokenUsage,
{
    let mut builder = client.agent(model);
    if !conversation.preamble.is_empty() {
        builder = builder.preamble(&conversation.preamble);
    }
    let agent = if tools.is_empty() {
        builder.build()
    } else {
        builder.tools(tools).build()
    };
    let inner = agent
        .stream_prompt(conversation.prompt)
        .with_history(conversation.history)
        .multi_turn(MAX_TURNS)
        .await;
    let mapped = inner.filter_map(move |item| {
        let subagent_names = Arc::clone(&subagent_names);
        async move {
            match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(t))) => {
                    Some(Ok(StreamEvent::Delta(t.text)))
                }
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
                Err(e) => Some(Err(CallError::Streaming(e.to_string()))),
            }
        }
    });
    Ok(Box::pin(mapped))
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
