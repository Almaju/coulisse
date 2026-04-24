use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::error_handling::HandleErrorLayer;
use axum::extract::State;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_oidc::error::MiddlewareError;
use axum_oidc::{EmptyAdditionalClaims, OidcAuthLayer, OidcLoginLayer};
use judge::{Judge, spawn_score};
use limits::Tracker;
use memory::{Memory, MemoryKind, MessageId, Role as MemRole, Store, UserId};
use prompter::Prompter;
use telemetry::{Ctx as TelemetryCtx, Event, EventKind, Sink as TelemetrySink, TurnId};
use time::Duration;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_sessions::cookie::SameSite;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};

use crate::admin_auth::{AdminAuth, require_basic_auth};
use crate::chat::Message as ChatMessage;
use crate::error::ApiError;
use crate::extractor::{Extractor, spawn_extract};
use crate::stream::{StreamContext, sse_response};
use crate::{ChatCompletionRequest, ServerError};

/// Shared state for the HTTP server. Held in an `Arc` so axum handlers can
/// cheaply clone the reference.
pub struct AppState<P: Prompter> {
    /// Admin-auth configuration. `None` leaves `/admin/*` unauthenticated —
    /// acceptable only on a loopback-only dev box or behind a reverse proxy
    /// that handles auth upstream. When `Some`, the variant picks the
    /// scheme: static Basic credentials or an OIDC login flow.
    pub admin_auth: Option<AdminAuth>,
    /// Fallback user id applied to requests that don't supply their own.
    /// `None` means such requests are rejected (multi-tenant posture).
    pub default_user_id: Option<UserId>,
    /// Optional auto-extraction configured via YAML. When `None`, the
    /// memories table is only written via explicit API calls.
    pub extractor: Option<Arc<Extractor>>,
    /// All judges configured in YAML, keyed by name. Agents opt in by listing
    /// judge names on themselves — the per-request handler looks up which of
    /// these apply to the agent being called.
    pub judges: Arc<HashMap<String, Arc<Judge>>>,
    pub memory: Arc<Store>,
    pub prompter: Arc<P>,
    pub telemetry: Arc<TelemetrySink>,
    pub tracker: Tracker,
}

/// Collect the judges configured for `agent_name`, preserving the order
/// declared on the agent so log output is stable. Unknown judge names are
/// skipped silently — validation at config load already rejects dangling
/// references, so any miss here is a programmer error, not user input.
pub(crate) fn judges_for_agent<P: Prompter>(
    state: &AppState<P>,
    agent_name: &str,
) -> Vec<Arc<Judge>> {
    let Some(agent) = state
        .prompter
        .agents()
        .iter()
        .find(|a| a.name == agent_name)
    else {
        return Vec::new();
    };
    agent
        .judges
        .iter()
        .filter_map(|name| state.judges.get(name).cloned())
        .collect()
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
        let state = Arc::clone(&self.state);

        // Assemble the full `/admin/*` subtree in one place so auth layers
        // cover every route uniformly: `/api/*` for JSON, everything else
        // for the Leptos SPA. Nested `/api` is more specific than the
        // SPA's `/{*path}` fallback, so axum routes correctly.
        let admin = Router::new()
            .nest("/api", crate::admin::router::<P>())
            .merge(crate::admin_ui::router::<P>());

        let admin = match state.admin_auth.as_ref() {
            None => admin,
            Some(AdminAuth::Basic(_)) => admin.route_layer(from_fn_with_state(
                Arc::clone(&state),
                require_basic_auth::<P>,
            )),
            Some(AdminAuth::Oidc(runtime)) => {
                // Session → auth (reads session, sets extensions) → login
                // (forces redirect when no valid ID token). `.layer()` calls
                // are applied outermost-last; session must wrap everything
                // so the OIDC layers find it in request extensions.
                // `HandleErrorLayer` converts the OIDC middlewares'
                // `MiddlewareError` into axum-compatible `Infallible`
                // responses.
                let session = SessionManagerLayer::new(MemoryStore::default())
                    .with_same_site(SameSite::Lax)
                    .with_expiry(Expiry::OnInactivity(Duration::hours(8)));
                let oidc_login = ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(handle_oidc_error))
                    .layer(OidcLoginLayer::<EmptyAdditionalClaims>::new());
                let oidc_auth = ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(handle_oidc_error))
                    .layer(OidcAuthLayer::<EmptyAdditionalClaims>::new(
                        runtime.client.clone(),
                    ));
                admin.layer(oidc_login).layer(oidc_auth).layer(session)
            }
        };

        Router::new()
            .nest("/admin", admin)
            .route("/v1/chat/completions", post(chat_completions::<P>))
            .route("/v1/models", get(models::<P>))
            .with_state(state)
    }
}

/// Turn an OIDC middleware error into an axum response. Session lookups,
/// token exchanges, and upstream discovery failures all funnel through
/// here. `MiddlewareError` already implements `IntoResponse`; the
/// wrapper exists only so the middleware stack resolves to axum's
/// `Infallible` error type.
async fn handle_oidc_error(err: MiddlewareError) -> Response {
    err.into_response()
}

async fn chat_completions<P: Prompter + 'static>(
    State(state): State<Arc<AppState<P>>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    let prepared = prepare_request(&state, &request).await?;

    // Pre-generate the assistant message id so we can reuse its UUID as the
    // telemetry turn correlation id. One value joins the stored message to
    // the full event tree produced while generating it.
    let assistant_message_id = MessageId::new();
    let turn_id = TurnId(assistant_message_id.0);
    let turn_start = Event::new(
        turn_id,
        prepared.user_id,
        None,
        EventKind::TurnStart,
        serde_json::json!({
            "agent": request.model,
            "user_message": prepared.user_message,
        }),
    );
    let turn_start_id = turn_start.id;
    if let Err(err) = state.telemetry.emit(turn_start).await {
        eprintln!("telemetry emit failed for turn_start: {err}");
    }
    let ctx = TelemetryCtx {
        correlation_id: turn_id,
        parent: Some(turn_start_id),
        user_id: prepared.user_id,
    };

    if request.is_streaming() {
        let inner = state
            .prompter
            .complete_streaming(&request.model, prepared.messages, ctx)
            .await?;
        let response = sse_response(StreamContext {
            assistant_message_id,
            include_usage: request.include_usage(),
            inner,
            model: request.model.clone(),
            state: Arc::clone(&state),
            tracker_key: prepared.tracker_key,
            user_id: prepared.user_id,
            user_message: prepared.user_message,
        });
        return Ok(response.into_response());
    }

    let completion = state
        .prompter
        .complete(&request.model, prepared.messages, ctx)
        .await?;
    if let Err(err) = state
        .tracker
        .record(&prepared.tracker_key, completion.usage.total_tokens)
        .await
    {
        eprintln!("rate limit record failed: {err}");
    }

    let um = state.memory.for_user(prepared.user_id);
    um.append_message(MemRole::User, prepared.user_message.clone())
        .await?;
    um.append_message_with_id(
        MemRole::Assistant,
        completion.text.clone(),
        assistant_message_id,
    )
    .await?;

    if let Some(extractor) = state.extractor.clone() {
        spawn_extract(
            extractor,
            Arc::clone(&state.memory),
            Arc::clone(&state.prompter),
            prepared.user_id,
            prepared.user_message.clone(),
            completion.text.clone(),
        );
    }

    let judges = judges_for_agent(&state, &request.model);
    spawn_score(
        judges,
        Arc::clone(&state.memory),
        Arc::clone(&state.prompter),
        prepared.user_id,
        assistant_message_id,
        prepared.user_message,
        completion.text.clone(),
    );

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
    state.tracker.check(&tracker_key, limits).await?;

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
