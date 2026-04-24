//! Real in-memory `Prompter` implementation for tests. Drives handlers
//! deterministically without talking to a provider.

use std::sync::Mutex;

use async_stream::stream;

use crate::{
    AgentConfig, Completion, CompletionStream, Message, Prompter, PrompterError, StreamEvent, Usage,
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
    pub usage: Usage,
}

impl ScriptedReply {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            deltas: vec![s.into()],
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
            for d in reply.deltas {
                yield Ok(StreamEvent::Delta(d));
            }
            yield Ok(StreamEvent::Done { usage: reply.usage });
        };
        Ok(Box::pin(s))
    }
}
