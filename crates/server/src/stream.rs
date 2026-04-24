use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use async_stream::stream;
use axum::response::Sse;
use axum::response::sse::{Event, KeepAlive};
use futures::StreamExt;
use memory::{Embedder, Role as MemRole, UserId};
use prompter::{CompletionStream, Prompter, StreamEvent, Usage as ProviderUsage};

use crate::chat::{
    ChatCompletionChunk, ChunkChoice, ChunkDelta, FinishReason, Role, Usage, now_secs, response_id,
};
use crate::server::AppState;

/// Build an SSE response from a stream of `StreamEvent`s. The handler keeps
/// the rest of the per-request state (user id, tracker key, user message)
/// alive through `MemoryFlush`, which writes back to memory and the rate
/// tracker on drop — so a client disconnect mid-stream still records the
/// partial assistant reply rather than losing both messages.
pub fn sse_response<E: Embedder + 'static, P: Prompter + 'static>(
    state: Arc<AppState<E, P>>,
    user_id: UserId,
    tracker_key: String,
    user_message: String,
    model: String,
    include_usage: bool,
    inner: CompletionStream,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let created = now_secs();
    let id = response_id(created);
    let accumulated: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let final_usage: Arc<Mutex<ProviderUsage>> = Arc::new(Mutex::new(ProviderUsage::default()));

    let flush = MemoryFlush {
        accumulated: Arc::clone(&accumulated),
        final_usage: Arc::clone(&final_usage),
        state,
        tracker_key,
        user_id,
        user_message,
    };

    let body = stream! {
        // Hold the flush guard inside the stream so Drop fires on either
        // normal completion or client disconnect.
        let _flush = flush;

        yield Ok::<_, Infallible>(role_chunk(&id, &model, created));

        let mut inner = inner;
        let mut errored = false;
        while let Some(event) = inner.next().await {
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
                Err(err) => {
                    yield Ok(error_chunk(&id, &model, created, &err.to_string()));
                    errored = true;
                    break;
                }
            }
        }

        if !errored {
            let usage = if include_usage {
                Some(Usage::from_prompter(*final_usage.lock().unwrap()))
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
struct MemoryFlush<E: Embedder + 'static, P: Prompter + 'static> {
    accumulated: Arc<Mutex<String>>,
    final_usage: Arc<Mutex<ProviderUsage>>,
    state: Arc<AppState<E, P>>,
    tracker_key: String,
    user_id: UserId,
    user_message: String,
}

impl<E: Embedder + 'static, P: Prompter + 'static> Drop for MemoryFlush<E, P> {
    fn drop(&mut self) {
        let accumulated = std::mem::take(&mut *self.accumulated.lock().unwrap());
        let usage = *self.final_usage.lock().unwrap();
        let state = Arc::clone(&self.state);
        let tracker_key = std::mem::take(&mut self.tracker_key);
        let user_id = self.user_id;
        let user_message = std::mem::take(&mut self.user_message);
        tokio::spawn(async move {
            state.tracker.record(&tracker_key, usage.total_tokens);
            let um = state.memory.for_user(user_id).await;
            if let Err(err) = um.append_message(MemRole::User, user_message).await {
                warn_memory_append_failed("user", err);
            }
            if !accumulated.is_empty()
                && let Err(err) = um.append_message(MemRole::Assistant, accumulated).await
            {
                warn_memory_append_failed("assistant", err);
            }
        });
    }
}

fn warn_memory_append_failed(role: &str, err: memory::MemoryError) {
    eprintln!("memory append failed for {role} message after streaming response: {err}");
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
