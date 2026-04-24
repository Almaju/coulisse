use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use limits::Tracker;
use memory::{Embedder, Memory, MemoryKind, Role as MemRole, Store, UserId};
use prompter::Prompter;
use tokio::net::TcpListener;

use crate::error::ApiError;
use crate::{ChatCompletionRequest, ChatCompletionResponse, ServerError};

/// Shared state for the HTTP server. Held in an `Arc` so axum handlers can
/// cheaply clone the reference.
pub struct AppState<E: Embedder> {
    /// Fallback user id applied to requests that don't supply their own.
    /// `None` means such requests are rejected (multi-tenant posture).
    pub default_user_id: Option<UserId>,
    pub memory: Store<E>,
    pub prompter: Prompter,
    pub tracker: Tracker,
}

pub struct Server<E: Embedder + 'static> {
    addr: SocketAddr,
    state: Arc<AppState<E>>,
}

impl<E: Embedder + 'static> Server<E> {
    pub fn new(addr: SocketAddr, state: Arc<AppState<E>>) -> Self {
        Self { addr, state }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn run(self) -> Result<(), ServerError> {
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(ServerError::Bind)?;
        axum::serve(listener, self.router())
            .await
            .map_err(ServerError::Serve)?;
        Ok(())
    }

    fn router(&self) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(Self::chat_completions))
            .route("/v1/models", get(Self::models))
            .with_state(Arc::clone(&self.state))
    }

    async fn chat_completions(
        State(state): State<Arc<AppState<E>>>,
        Json(request): Json<ChatCompletionRequest>,
    ) -> Result<Json<ChatCompletionResponse>, ApiError> {
        let user_id = request.user_id().or(state.default_user_id).ok_or_else(|| {
            ApiError::BadRequest(
                "missing user identifier: set `safety_identifier` (preferred) or the deprecated `user` field"
                    .into(),
            )
        })?;
        let limits = request.request_limits()?;
        let tracker_key = user_id.0.to_string();
        state.tracker.check(&tracker_key, limits)?;

        let last_user = request
            .last_user_message()
            .ok_or_else(|| ApiError::BadRequest("no user message to respond to".into()))?;
        let um = state.memory.for_user(user_id).await;
        let budget = state.memory.config().context_budget;
        let assembled = um
            .assemble_context(last_user.content_or_empty(), budget)
            .await?;

        let mut messages: Vec<prompter::Message> = Vec::new();
        for sys in request.system_messages() {
            messages.push(prompter::Message {
                content: sys.content_or_empty().to_string(),
                role: prompter::Role::System,
            });
        }
        if !assembled.memories.is_empty() {
            messages.push(prompter::Message {
                content: format_memory_block(&assembled.memories),
                role: prompter::Role::System,
            });
        }
        for m in assembled.messages {
            messages.push(prompter::Message {
                content: m.content,
                role: match m.role {
                    MemRole::Assistant => prompter::Role::Assistant,
                    MemRole::System => prompter::Role::System,
                    MemRole::User => prompter::Role::User,
                },
            });
        }
        messages.push(prompter::Message {
            content: last_user.content_or_empty().to_string(),
            role: prompter::Role::User,
        });

        let completion = state.prompter.complete(&request.model, messages).await?;
        state
            .tracker
            .record(&tracker_key, completion.usage.total_tokens);

        um.append_message(MemRole::User, last_user.content_or_empty().to_string())
            .await?;
        um.append_message(MemRole::Assistant, completion.text.clone())
            .await?;

        Ok(Json(
            request.response_with(completion.text, completion.usage),
        ))
    }

    async fn models(State(state): State<Arc<AppState<E>>>) -> Json<serde_json::Value> {
        let data: Vec<_> = state
            .prompter
            .agents()
            .iter()
            .map(|agent| {
                serde_json::json!({
                    "created": 0,
                    "id": agent.name,
                    "object": "model",
                    "owned_by": agent.provider.as_str(),
                })
            })
            .collect();
        Json(serde_json::json!({
            "data": data,
            "object": "list",
        }))
    }
}

fn format_memory_block(memories: &[Memory]) -> String {
    let mut out = String::from("Known about the user:\n");
    for m in memories {
        let tag = match m.kind {
            MemoryKind::Fact => "fact",
            MemoryKind::Preference => "preference",
        };
        out.push_str(&format!("- [{tag}] {}\n", m.content));
    }
    out
}
