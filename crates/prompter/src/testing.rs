//! Real in-memory `Prompter` implementation for tests. Drives handlers
//! deterministically without talking to a provider.

use std::sync::Mutex;

use async_stream::stream;

use crate::{
    AgentConfig, Completion, CompletionStream, Message, Prompter, PrompterError, ProviderKind,
    StreamEvent, ToolCallKind, Usage,
};

/// A `Prompter` that replays a scripted reply. Each call to `complete` or
/// `complete_streaming` captures the incoming messages in `calls` and then
/// returns the next scripted response (or loops on the last one).
pub struct ScriptedPrompter {
    agents: Vec<AgentConfig>,
    calls: Mutex<Vec<Vec<Message>>>,
    replies: Mutex<Vec<ScriptedReply>>,
}

#[derive(Clone)]
pub struct ScriptedReply {
    pub deltas: Vec<String>,
    pub tool_calls: Vec<ScriptedToolCall>,
    pub usage: Usage,
}

/// One scripted tool invocation emitted during a streaming reply. Fires
/// before the text deltas so tests can assert that the server correlates
/// call + result into the admin trail.
#[derive(Clone)]
pub struct ScriptedToolCall {
    pub args: String,
    pub call_id: String,
    pub kind: ToolCallKind,
    pub result: Option<String>,
    pub tool_name: String,
}

impl ScriptedReply {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            deltas: vec![s.into()],
            tool_calls: Vec::new(),
            usage: Usage {
                output_tokens: 1,
                total_tokens: 1,
                ..Usage::default()
            },
        }
    }

    pub fn deltas<I, S>(deltas: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            deltas: deltas.into_iter().map(Into::into).collect(),
            tool_calls: Vec::new(),
            usage: Usage {
                output_tokens: 1,
                total_tokens: 1,
                ..Usage::default()
            },
        }
    }

    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = usage;
        self
    }

    /// Attach a scripted tool call to this reply. In the streaming path, the
    /// call event is emitted before the text deltas, followed by the paired
    /// result event (when `result` is `Some`). The non-streaming `complete`
    /// path ignores tool calls — capture is streaming-only for now.
    pub fn with_tool_call(
        mut self,
        tool_name: impl Into<String>,
        args: impl Into<String>,
        kind: ToolCallKind,
        result: Option<String>,
    ) -> Self {
        let call_id = format!("scripted-{}", self.tool_calls.len());
        self.tool_calls.push(ScriptedToolCall {
            args: args.into(),
            call_id,
            kind,
            result,
            tool_name: tool_name.into(),
        });
        self
    }

    fn full_text(&self) -> String {
        self.deltas.concat()
    }
}

impl ScriptedPrompter {
    pub fn new(agents: Vec<AgentConfig>, replies: Vec<ScriptedReply>) -> Self {
        Self {
            agents,
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(replies),
        }
    }

    /// Messages received on each `complete`/`complete_streaming` call so far,
    /// in call order. Useful for asserting that the handler assembled the
    /// context correctly.
    pub fn calls(&self) -> Vec<Vec<Message>> {
        self.calls.lock().unwrap().clone()
    }

    fn next_reply(&self, agent_name: &str) -> Result<ScriptedReply, PrompterError> {
        if !self.agents.iter().any(|a| a.name == agent_name) {
            return Err(PrompterError::UnknownAgent(agent_name.to_string()));
        }
        let mut replies = self.replies.lock().unwrap();
        match replies.len() {
            0 => Err(PrompterError::Streaming(
                "scripted prompter has no replies left".into(),
            )),
            1 => Ok(replies[0].clone()),
            _ => Ok(replies.remove(0)),
        }
    }
}

impl Prompter for ScriptedPrompter {
    fn agents(&self) -> &[AgentConfig] {
        &self.agents
    }

    async fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
    ) -> Result<Completion, PrompterError> {
        self.calls.lock().unwrap().push(messages);
        let reply = self.next_reply(agent_name)?;
        Ok(Completion {
            text: reply.full_text(),
            usage: reply.usage,
        })
    }

    async fn complete_streaming(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
    ) -> Result<CompletionStream, PrompterError> {
        self.calls.lock().unwrap().push(messages);
        let reply = self.next_reply(agent_name)?;
        let s = stream! {
            for tc in reply.tool_calls {
                yield Ok(StreamEvent::ToolCall {
                    args: tc.args,
                    call_id: tc.call_id.clone(),
                    kind: tc.kind,
                    tool_name: tc.tool_name,
                });
                if let Some(result) = tc.result {
                    yield Ok(StreamEvent::ToolResult {
                        call_id: tc.call_id,
                        error: None,
                        result: Some(result),
                    });
                }
            }
            for d in reply.deltas {
                yield Ok(StreamEvent::Delta(d));
            }
            yield Ok(StreamEvent::Done { usage: reply.usage });
        };
        Ok(Box::pin(s))
    }

    async fn prompt_with(
        &self,
        _provider: ProviderKind,
        _model: &str,
        _preamble: &str,
        messages: Vec<Message>,
    ) -> Result<Completion, PrompterError> {
        self.calls.lock().unwrap().push(messages);
        let reply = {
            let mut replies = self.replies.lock().unwrap();
            match replies.len() {
                0 => {
                    return Err(PrompterError::Streaming(
                        "scripted prompter has no replies left".into(),
                    ));
                }
                1 => replies[0].clone(),
                _ => replies.remove(0),
            }
        };
        Ok(Completion {
            text: reply.full_text(),
            usage: reply.usage,
        })
    }
}
