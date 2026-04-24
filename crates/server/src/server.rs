use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use limits::Tracker;
use memory::{Memory, MemoryKind, Role as MemRole, Store, UserId};
use prompter::Prompter;
use tokio::net::TcpListener;

use crate::chat::Message as ChatMessage;
use crate::error::ApiError;
use crate::extractor::{Extractor, spawn_extract};
use crate::stream::sse_response;
use crate::{ChatCompletionRequest, ServerError};

/// Shared state for the HTTP server. Held in an `Arc` so axum handlers can
/// cheaply clone the reference.
pub struct AppState<P: Prompter> {
    /// Fallback user id applied to requests that don't supply their own.
    /// `None` means such requests are rejected (multi-tenant posture).
    pub default_user_id: Option<UserId>,
    /// Optional auto-extraction configured via YAML. When `None`, the
    /// memories table is only written via explicit API calls.
    pub extractor: Option<Arc<Extractor>>,
    pub memory: Arc<Store>,
    pub prompter: Arc<P>,
    pub tracker: Tracker,
}

pub struct Server<P: Prompter + 'static> {
    addr: SocketAddr,
    state: Arc<AppState<P>>,
}

impl<P: Prompter + 'static> Server<P> {
    pub fn new(addr: SocketAddr, state: Arc<AppState<P>>) -> Self {
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

    /// Public so integration tests can drive the router via `tower::ServiceExt`
    /// without standing up a TCP listener.
    pub fn router(&self) -> Router {
        Router::new()
            .nest("/admin/api", crate::admin::router::<P>())
            .nest("/admin", crate::admin_ui::router::<P>())
            .route("/v1/chat/completions", post(chat_completions::<P>))
            .route("/v1/models", get(models::<P>))
            .with_state(Arc::clone(&self.state))
    }
}

async fn chat_completions<P: Prompter + 'static>(
    State(state): State<Arc<AppState<P>>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    let prepared = prepare_request(&state, &request).await?;

    if request.is_streaming() {
        let inner = state
            .prompter
            .complete_streaming(&request.model, prepared.messages)
            .await?;
        let response = sse_response(
            Arc::clone(&state),
            prepared.user_id,
            prepared.tracker_key,
            prepared.user_message,
            request.model.clone(),
            request.include_usage(),
            inner,
        );
        return Ok(response.into_response());
    }

    let completion = state
        .prompter
        .complete(&request.model, prepared.messages)
        .await?;
    state
        .tracker
        .record(&prepared.tracker_key, completion.usage.total_tokens);

    let um = state.memory.for_user(prepared.user_id);
    um.append_message(MemRole::User, prepared.user_message.clone())
        .await?;
    um.append_message(MemRole::Assistant, completion.text.clone())
        .await?;

    if let Some(extractor) = state.extractor.clone() {
        spawn_extract(
            extractor,
            Arc::clone(&state.memory),
            Arc::clone(&state.prompter),
            prepared.user_id,
            prepared.user_message,
            completion.text.clone(),
        );
    }

    Ok(Json(request.response_with(completion.text, completion.usage)).into_response())
}

async fn models<P: Prompter>(State(state): State<Arc<AppState<P>>>) -> Json<serde_json::Value> {
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

/// Per-request derived state shared by the streaming and non-streaming
/// branches: which user this is, their rate-limit key, the new user message,
/// and the assembled context to forward to the model.
struct PreparedRequest {
    messages: Vec<prompter::Message>,
    tracker_key: String,
    user_id: UserId,
    user_message: String,
}

async fn prepare_request<P: Prompter>(
    state: &Arc<AppState<P>>,
    request: &ChatCompletionRequest,
) -> Result<PreparedRequest, ApiError> {
    let user_id = request.user_id().or(state.default_user_id).ok_or_else(|| {
        ApiError::BadRequest(
            "missing user identifier: set `safety_identifier` (preferred) or the deprecated `user` field"
                .into(),
        )
    })?;
    let limits = request.request_limits()?;
    let language = request.language()?;
    let tracker_key = user_id.0.to_string();
    state.tracker.check(&tracker_key, limits)?;

    let last_user: &ChatMessage = request
        .last_user_message()
        .ok_or_else(|| ApiError::BadRequest("no user message to respond to".into()))?;
    let user_message = last_user.content_or_empty().to_string();
    let um = state.memory.for_user(user_id);
    let budget = state.memory.config().context_budget;
    let assembled = um.assemble_context(&user_message, budget).await?;

    let mut messages: Vec<prompter::Message> = Vec::new();
    if let Some(tag) = language {
        messages.push(prompter::Message {
            content: tag.instruction(),
            role: prompter::Role::System,
        });
    }
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
        content: user_message.clone(),
        role: prompter::Role::User,
    });

    Ok(PreparedRequest {
        messages,
        tracker_key,
        user_id,
        user_message,
    })
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
