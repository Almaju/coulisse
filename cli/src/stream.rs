use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use agents::{Agents, CompletionStream, StreamEvent, Usage as ProviderUsage};
use async_stream::stream;
use axum::response::Sse;
use axum::response::sse::{Event, KeepAlive};
use coulisse_core::OneShotPrompt;
use futures::StreamExt;
use judges::spawn_score;
use memory::{MessageId, Role as MemRole, UserId};
use tracing::{Instrument, Span};

use proxy::{
    ChatCompletionChunk, ChunkChoice, ChunkDelta, FinishReason, Role, Usage, now_secs, response_id,
};

use crate::server::{AppState, judges_for_agent};

/// Build an SSE response from a stream of `StreamEvent`s. The handler keeps
/// the rest of the per-request state (user id, tracker key, user message)
/// alive through `MemoryFlush`, which writes back to memory and the rate
/// tracker on drop — so a client disconnect mid-stream still records the
/// partial assistant reply rather than losing both messages.
/// Parameters for `sse_response`. Bundled so the function's argument list
/// stays under clippy's `too_many_arguments` lint, and so new per-request
/// fields (telemetry turn id, future flags) can be added without breaking
/// callers.
pub struct StreamContext<P: Agents + OneShotPrompt + 'static> {
    /// Resolved agent name — what judges score and what `judges_for_agent`
    /// looks up. Differs from `model` when the request hit an experiment:
    /// `model` echoes back the experiment name the client sent, while
    /// `agent_name` records which variant actually ran.
    pub agent_name: String,
    pub assistant_message_id: MessageId,
    pub include_usage: bool,
    pub inner: CompletionStream,
    pub model: String,
    pub state: Arc<AppState<P>>,
    pub tracker_key: String,
    /// Root `turn` span the SSE body runs inside. Drives `Span::current()`
    /// for any post-stream side effects so memory writes / score jobs
    /// share the same correlation id as the LLM work.
    pub turn_span: Span,
    pub user_id: UserId,
    pub user_message: String,
}

pub fn sse_response<P: Agents + OneShotPrompt + 'static>(
    cx: StreamContext<P>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let StreamContext {
        agent_name,
        assistant_message_id,
        include_usage,
        inner,
        model,
        state,
        tracker_key,
        turn_span,
        user_id,
        user_message,
    } = cx;
    let created = now_secs();
    let id = response_id(created);
    let accumulated: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let final_usage: Arc<Mutex<ProviderUsage>> = Arc::new(Mutex::new(ProviderUsage::default()));

    let flush = MemoryFlush {
        accumulated: Arc::clone(&accumulated),
        agent_name,
        assistant_message_id,
        final_usage: Arc::clone(&final_usage),
        state,
        tracker_key,
        turn_span: turn_span.clone(),
        user_id,
        user_message,
    };

    let stream_span = turn_span.clone();
    let body = stream! {
        // Hold the flush guard inside the stream so Drop fires on either
        // normal completion or client disconnect.
        let _flush = flush;

        yield Ok::<_, Infallible>(role_chunk(&id, &model, created));

        let mut inner = inner;
        let mut errored = false;
        // Tool-call observability is owned by the agents-side wrappers
        // (which emit `tool_call` spans the SqliteLayer mirrors into
        // events / tool_calls). The streaming path only needs to forward
        // SSE deltas and the terminal stop chunk to the client.
        // Each `inner.next()` poll runs inside `stream_span` so any
        // `tool_call` spans rig drives during that poll nest under it.
        loop {
            let event = inner.next().instrument(stream_span.clone()).await;
            let Some(event) = event else { break };
            match event {
                Ok(StreamEvent::Delta(text)) => {
                    if !text.is_empty() {
                        accumulated.lock().unwrap().push_str(&text);
                        yield Ok(content_chunk(&id, &model, created, &text));
                    }
                }
                Ok(StreamEvent::Done { usage }) => {
                    *final_usage.lock().unwrap() = usage;
                }
                Ok(StreamEvent::ToolCall { .. } | StreamEvent::ToolResult { .. }) => {}
                Err(err) => {
                    yield Ok(error_chunk(&id, &model, created, &err.to_string()));
                    errored = true;
                    break;
                }
            }
        }

        if !errored {
            let usage = if include_usage {
                let u = *final_usage.lock().unwrap();
                Some(Usage::new(u.input_tokens, u.output_tokens, u.total_tokens))
            } else {
                None
            };
            yield Ok(stop_chunk(&id, &model, created, usage));
        }
        yield Ok(Event::default().data("[DONE]"));
    };

    Sse::new(body).keep_alive(KeepAlive::default())
}

/// Drop guard: persists the conversation to memory and records token usage
/// when the SSE stream ends, regardless of whether it ended normally or the
/// client disconnected. Spawned onto the runtime because `Drop` is sync.
struct MemoryFlush<P: Agents + OneShotPrompt + 'static> {
    accumulated: Arc<Mutex<String>>,
    /// Resolved agent name; used for `judges_for_agent`. See
    /// `StreamContext::agent_name` for the rationale.
    agent_name: String,
    assistant_message_id: MessageId,
    final_usage: Arc<Mutex<ProviderUsage>>,
    state: Arc<AppState<P>>,
    tracker_key: String,
    /// Root `turn` span; spawned cleanup work runs inside it so its
    /// own warnings carry the same correlation id.
    turn_span: Span,
    user_id: UserId,
    user_message: String,
}

impl<P: Agents + OneShotPrompt + 'static> Drop for MemoryFlush<P> {
    fn drop(&mut self) {
        let accumulated = std::mem::take(&mut *self.accumulated.lock().unwrap());
        let agent_name = std::mem::take(&mut self.agent_name);
        let assistant_message_id = self.assistant_message_id;
        let usage = *self.final_usage.lock().unwrap();
        let state = Arc::clone(&self.state);
        let tracker_key = std::mem::take(&mut self.tracker_key);
        let turn_span = self.turn_span.clone();
        let user_id = self.user_id;
        let user_message = std::mem::take(&mut self.user_message);
        tokio::spawn(
            async move {
                if let Err(err) = state.tracker.record(&tracker_key, usage.total_tokens).await {
                    tracing::warn!(error = %err, "rate limit record failed after streaming response");
                }
                let um = state.memory.for_user(user_id);
                if let Err(err) = um.append_message(MemRole::User, user_message.clone()).await {
                    warn_memory_append_failed("user", err);
                }
                if accumulated.is_empty() {
                    return;
                }
                let assistant_append = um
                    .append_message_with_id(
                        MemRole::Assistant,
                        accumulated.clone(),
                        assistant_message_id,
                    )
                    .await;
                if let Err(err) = assistant_append {
                    warn_memory_append_failed("assistant", err);
                    return;
                }
                if let Some(extractor) = state.extractor.as_ref() {
                    extractor.spawn(
                        Arc::clone(&state.memory),
                        user_id,
                        user_message.clone(),
                        accumulated.clone(),
                    );
                }
                let judges = judges_for_agent(&state, &agent_name);
                spawn_score(
                    judges,
                    Arc::clone(&state.judge_store),
                    Arc::clone(&state.agents),
                    user_id,
                    assistant_message_id,
                    agent_name.clone(),
                    user_message,
                    accumulated,
                );
            }
            .instrument(turn_span),
        );
    }
}

fn warn_memory_append_failed(role: &str, err: memory::MemoryError) {
    tracing::warn!(role, error = %err, "memory append failed after streaming response");
}

fn chunk_event(chunk: &ChatCompletionChunk) -> Event {
    Event::default().json_data(chunk).expect("chunk serializes")
}

fn role_chunk(id: &str, model: &str, created: u64) -> Event {
    chunk_event(&ChatCompletionChunk {
        choices: vec![ChunkChoice {
            delta: ChunkDelta {
                content: None,
                role: Some(Role::Assistant),
            },
            finish_reason: None,
            index: 0,
        }],
        created,
        id: id.to_string(),
        model: model.to_string(),
        object: "chat.completion.chunk".into(),
        usage: None,
    })
}

fn content_chunk(id: &str, model: &str, created: u64, text: &str) -> Event {
    chunk_event(&ChatCompletionChunk {
        choices: vec![ChunkChoice {
            delta: ChunkDelta {
                content: Some(text.to_string()),
                role: None,
            },
            finish_reason: None,
            index: 0,
        }],
        created,
        id: id.to_string(),
        model: model.to_string(),
        object: "chat.completion.chunk".into(),
        usage: None,
    })
}

fn stop_chunk(id: &str, model: &str, created: u64, usage: Option<Usage>) -> Event {
    chunk_event(&ChatCompletionChunk {
        choices: vec![ChunkChoice {
            delta: ChunkDelta::default(),
            finish_reason: Some(FinishReason::Stop),
            index: 0,
        }],
        created,
        id: id.to_string(),
        model: model.to_string(),
        object: "chat.completion.chunk".into(),
        usage,
    })
}

fn error_chunk(id: &str, model: &str, created: u64, message: &str) -> Event {
    Event::default()
        .json_data(serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": "stop",
                "index": 0,
            }],
            "created": created,
            "error": { "message": message, "type": "upstream_error" },
            "id": id,
            "model": model,
            "object": "chat.completion.chunk",
        }))
        .expect("error chunk serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_chunk_announces_assistant_with_no_finish_reason() {
        let chunk = ChatCompletionChunk {
            choices: vec![ChunkChoice {
                delta: ChunkDelta {
                    content: None,
                    role: Some(Role::Assistant),
                },
                finish_reason: None,
                index: 0,
            }],
            created: 42,
            id: "chatcmpl-coulisse-42".into(),
            model: "agent".into(),
            object: "chat.completion.chunk".into(),
            usage: None,
        };
        let v = serde_json::to_value(&chunk).unwrap();
        assert_eq!(v["object"], "chat.completion.chunk");
        assert_eq!(v["choices"][0]["delta"]["role"], "assistant");
        assert!(v["choices"][0]["delta"].get("content").is_none());
        assert!(v["choices"][0].get("finish_reason").is_none());
        assert!(v.get("usage").is_none());
    }

    #[test]
    fn content_chunk_carries_only_content_in_delta() {
        let chunk = ChatCompletionChunk {
            choices: vec![ChunkChoice {
                delta: ChunkDelta {
                    content: Some("hi".into()),
                    role: None,
                },
                finish_reason: None,
                index: 0,
            }],
            created: 42,
            id: "x".into(),
            model: "m".into(),
            object: "chat.completion.chunk".into(),
            usage: None,
        };
        let v = serde_json::to_value(&chunk).unwrap();
        assert_eq!(v["choices"][0]["delta"]["content"], "hi");
        assert!(v["choices"][0]["delta"].get("role").is_none());
    }

    #[test]
    fn stop_chunk_includes_finish_reason_and_optional_usage() {
        let with_usage = ChatCompletionChunk {
            choices: vec![ChunkChoice {
                delta: ChunkDelta::default(),
                finish_reason: Some(FinishReason::Stop),
                index: 0,
            }],
            created: 42,
            id: "x".into(),
            model: "m".into(),
            object: "chat.completion.chunk".into(),
            usage: Some(Usage {
                completion_tokens: 3,
                prompt_tokens: 7,
                total_tokens: 10,
            }),
        };
        let v = serde_json::to_value(&with_usage).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert_eq!(v["usage"]["total_tokens"], 10);

        let without = ChatCompletionChunk {
            usage: None,
            ..with_usage
        };
        let v2 = serde_json::to_value(&without).unwrap();
        assert!(v2.get("usage").is_none());
    }
}
