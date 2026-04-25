use std::collections::HashMap;
use std::sync::Arc;

use agents::Agents;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use config::Strategy;
use judge::{Judge, spawn_score};
use limits::Tracker;
use memory::{Extractor, Memory, MemoryKind, MessageId, Role as MemRole, Store, UserId};
use telemetry::{Ctx as TelemetryCtx, Event, EventKind, Sink as TelemetrySink, TurnId};

use proxy::ChatCompletionRequest;
use proxy::Message as ChatMessage;

use crate::error::ApiError;
use crate::shadow::spawn_shadow_runs;
use crate::stream::{StreamContext, sse_response};

/// Shared state for the OpenAI-compatible proxy. Held in an `Arc` so axum
/// handlers can cheaply clone the reference.
pub struct AppState<P: Agents> {
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
    pub agents: Arc<P>,
    pub telemetry: Arc<TelemetrySink>,
    pub tracker: Tracker,
}

/// Build the OpenAI-compatible router. The cli composes this with other
/// routers (e.g. the studio UI) before binding a listener.
pub fn router<P: Agents + 'static>(state: Arc<AppState<P>>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions::<P>))
        .route("/v1/models", get(models::<P>))
        .with_state(state)
}

/// Collect the judges configured for `agent_name`, preserving the order
/// declared on the agent so log output is stable. Unknown judge names are
/// skipped silently — validation at config load already rejects dangling
/// references, so any miss here is a programmer error, not user input.
pub(crate) fn judges_for_agent<P: Agents>(
    state: &AppState<P>,
    agent_name: &str,
) -> Vec<Arc<Judge>> {
    let Some(agent) = state.agents.agents().iter().find(|a| a.name == agent_name) else {
        return Vec::new();
    };
    agent
        .judges
        .iter()
        .filter_map(|name| state.judges.get(name).cloned())
        .collect()
}

async fn chat_completions<P: Agents + 'static>(
    State(state): State<Arc<AppState<P>>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    let prepared = prepare_request(&state, &request).await?;

    // Resolve the addressable name (an agent or an experiment) to a
    // concrete variant agent for this user. Sticky-by-user routing means
    // the same user lands on the same variant across requests when
    // configured that way. The resolved name is what the prompter actually
    // runs, what judges score, and what telemetry records — but the
    // client-facing `model` echoed back in the response stays as the
    // user wrote it (so OpenAI clients see the model id they sent).
    //
    // For a bandit experiment the resolution requires recent mean
    // scores; for split/shadow/passthrough the lookup is a no-op.
    let bandit_scores = match state.agents.router().bandit_query(&request.model) {
        Some((judge, criterion, since)) => state
            .memory
            .mean_scores_by_agent(&judge, &criterion, since)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let resolved =
        state
            .agents
            .router()
            .resolve_with_scores(&request.model, prepared.user_id, &bandit_scores);
    let agent_name = resolved.agent.clone().into_owned();
    let experiment_name = resolved.experiment.map(str::to_owned);

    // Pre-generate the assistant message id so we can reuse its UUID as the
    // telemetry turn correlation id. One value joins the stored message to
    // the full event tree produced while generating it.
    let assistant_message_id = MessageId::new();
    let turn_id = TurnId(assistant_message_id.0);
    let turn_start_payload = match experiment_name.as_deref() {
        Some(experiment) => serde_json::json!({
            "agent": agent_name,
            "experiment": experiment,
            "user_message": prepared.user_message,
            "variant": agent_name,
        }),
        None => serde_json::json!({
            "agent": agent_name,
            "user_message": prepared.user_message,
        }),
    };
    let turn_start = Event::new(
        turn_id,
        prepared.user_id,
        None,
        EventKind::TurnStart,
        turn_start_payload,
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

    // Shadow setup: clone the inputs the primary will consume so the
    // background variants can run against the same context. No-op for
    // non-shadow strategies — the helper itself early-exits when the
    // experiment isn't shadow or sampling drops the turn.
    let shadow_inputs = state
        .agents
        .router()
        .get(&request.model)
        .filter(|exp| matches!(exp.strategy, Strategy::Shadow))
        .map(|exp| (exp.clone(), prepared.messages.clone()));

    if request.is_streaming() {
        if let Some((experiment, messages)) = shadow_inputs {
            spawn_shadow_runs(
                Arc::clone(&state),
                &experiment,
                turn_id,
                prepared.user_id,
                prepared.user_message.clone(),
                messages,
            );
        }
        let inner = state
            .agents
            .complete_streaming(&agent_name, prepared.messages, ctx)
            .await?;
        let response = sse_response(StreamContext {
            agent_name,
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
        .agents
        .complete(&agent_name, prepared.messages, ctx)
        .await?;
    if let Some((experiment, messages)) = shadow_inputs {
        spawn_shadow_runs(
            Arc::clone(&state),
            &experiment,
            turn_id,
            prepared.user_id,
            prepared.user_message.clone(),
            messages,
        );
    }
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

    if let Some(extractor) = state.extractor.as_ref() {
        extractor.spawn(
            Arc::clone(&state.memory),
            prepared.user_id,
            prepared.user_message.clone(),
            completion.text.clone(),
        );
    }

    let judges = judges_for_agent(&state, &agent_name);
    spawn_score(
        judges,
        Arc::clone(&state.memory),
        Arc::clone(&state.agents),
        prepared.user_id,
        assistant_message_id,
        agent_name.clone(),
        prepared.user_message,
        completion.text.clone(),
    );

    Ok(Json(request.response_with(completion.text, completion.usage)).into_response())
}

async fn models<P: Agents>(State(state): State<Arc<AppState<P>>>) -> Json<serde_json::Value> {
    let data: Vec<_> = state
        .agents
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
    messages: Vec<agents::Message>,
    tracker_key: String,
    user_id: UserId,
    user_message: String,
}

async fn prepare_request<P: Agents>(
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

    let mut messages: Vec<agents::Message> = Vec::new();
    if let Some(tag) = language {
        messages.push(agents::Message {
            content: tag.instruction(),
            role: agents::Role::System,
        });
    }
    for sys in request.system_messages() {
        messages.push(agents::Message {
            content: sys.content_or_empty().to_string(),
            role: agents::Role::System,
        });
    }
    if !assembled.memories.is_empty() {
        messages.push(agents::Message {
            content: format_memory_block(&assembled.memories),
            role: agents::Role::System,
        });
    }
    for m in assembled.messages {
        messages.push(agents::Message {
            content: m.content,
            role: match m.role {
                MemRole::Assistant => agents::Role::Assistant,
                MemRole::System => agents::Role::System,
                MemRole::User => agents::Role::User,
            },
        });
    }
    messages.push(agents::Message {
        content: user_message.clone(),
        role: agents::Role::User,
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
