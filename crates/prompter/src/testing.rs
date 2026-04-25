//! Real in-memory `Prompter` implementation for tests. Drives handlers
//! deterministically without talking to a provider.

use std::pin::Pin;
use std::sync::Mutex;

use async_stream::stream;

use config::{AgentConfig, ExperimentConfig, ProviderKind};
use coulisse_core::{OneShotError, OneShotPrompt};

use crate::experiment::ExperimentRouter;
use crate::{
    Completion, CompletionStream, Message, Prompter, PrompterError, Role, StreamEvent,
    ToolCallKind, Usage,
};

/// A `Prompter` that replays a scripted reply. Each call to `complete` or
/// `complete_streaming` captures the incoming messages in `calls` and the
/// dispatched agent name in `dispatched_to`, then returns the next
/// scripted response (or loops on the last one).
pub struct ScriptedPrompter {
    agents: Vec<AgentConfig>,
    calls: Mutex<Vec<Vec<Message>>>,
    dispatched_to: Mutex<Vec<String>>,
    replies: Mutex<Vec<ScriptedReply>>,
    router: ExperimentRouter,
}

#[derive(Clone)]
pub struct ScriptedReply {
    pub deltas: Vec<String>,
    pub tool_calls: Vec<ScriptedToolCall>,
    pub usage: Usage,
}

/// One scripted tool invocation emitted during a streaming reply. Fires
/// before the text deltas so tests can assert that the server correlates
/// call + result into the studio trail.
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
        Self::with_experiments(agents, Vec::new(), replies)
    }

    pub fn with_experiments(
        agents: Vec<AgentConfig>,
        experiments: Vec<ExperimentConfig>,
        replies: Vec<ScriptedReply>,
    ) -> Self {
        Self {
            agents,
            calls: Mutex::new(Vec::new()),
            dispatched_to: Mutex::new(Vec::new()),
            replies: Mutex::new(replies),
            router: ExperimentRouter::new(experiments),
        }
    }

    /// Agent names the prompter was asked to run, in call order. Lets
    /// tests verify experiment routing — if the proxy resolved
    /// `model: alice` to variant `alice-v1`, this records `alice-v1`,
    /// not `alice`.
    pub fn dispatched_to(&self) -> Vec<String> {
        self.dispatched_to.lock().unwrap().clone()
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
        self.dispatched_to
            .lock()
            .unwrap()
            .push(agent_name.to_string());
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

    fn router(&self) -> &ExperimentRouter {
        &self.router
    }

    async fn complete(
        &self,
        agent_name: &str,
        messages: Vec<Message>,
        _ctx: telemetry::Ctx,
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
        _ctx: telemetry::Ctx,
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

impl OneShotPrompt for ScriptedPrompter {
    fn one_shot<'a>(
        &'a self,
        _provider: &'a str,
        _model: &'a str,
        _preamble: &'a str,
        user_text: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, OneShotError>> + Send + 'a>> {
        Box::pin(async move {
            let messages = vec![Message {
                content: user_text.to_string(),
                role: Role::User,
            }];
            self.prompt_with(ProviderKind::Openai, "scripted", "", messages)
                .await
                .map(|c| c.text)
                .map_err(|e| OneShotError::new(e.to_string()))
        })
    }
}
