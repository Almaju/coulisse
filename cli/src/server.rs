use std::collections::HashMap;
use std::sync::Arc;

use agents::Agents;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coulisse_core::OneShotPrompt;
use experiments::{ExperimentRouter, Strategy};
use judges::{Judge, Judges, spawn_score};
use limits::{RequestLimits, Tracker};
use memory::{Extractor, Memory, MemoryKind, MessageId, Role as MemRole, Store, UserId};
use telemetry::TurnId;
use tracing::{Instrument, Span, info_span};

use proxy::ChatCompletionRequest;
use proxy::Message as ChatMessage;

use crate::config::Users;
use crate::error::ApiError;
use crate::shadow::spawn_shadow_runs;
use crate::stream::{StreamContext, sse_response};

/// Hardcoded identity used in `Users::Shared` mode. Stable across
/// restarts because `UserId::from_string` derives the same UUID v5 from
/// the same input — and reserved enough that no real client will collide
/// with it. Memory and rate-limit counters are scoped to this id.
const SHARED_USER_ID: &str = "main";

/// Shared state for the OpenAI-compatible proxy. Held in an `Arc` so axum
/// handlers can cheaply clone the reference.
pub struct AppState<P: Agents + OneShotPrompt> {
    pub agents: Arc<P>,
    /// A/B routing table. Cli does top-level experiment resolution here
    /// (before calling `agents.complete`); agents itself never sees this
    /// — it asks an `AgentResolver` for subagent dispatch instead.
    pub experiments: Arc<ExperimentRouter>,
    /// Optional auto-extraction configured via YAML. When `None`, the
    /// memories table is only written via explicit API calls.
    pub extractor: Option<Arc<Extractor>>,
    /// Persistent score store owned by the judge crate. Reads (for
    /// bandit aggregates) and writes (from background scoring tasks)
    /// both go here.
    pub judge_store: Arc<Judges>,
    /// All judges configured in YAML, keyed by name. Agents opt in by listing
    /// judge names on themselves — the per-request handler looks up which of
    /// these apply to the agent being called.
    pub judges: Arc<HashMap<String, Arc<Judge>>>,
    pub memory: Arc<Store>,
    pub tracker: Tracker,
    /// User identification mode. `Shared` collapses every request onto
    /// `SHARED_USER_ID`; `PerRequest` requires the client to send
    /// `safety_identifier` (or the deprecated `user` field).
    pub users: Users,
}

pub fn router<P: Agents + OneShotPrompt + 'static>(state: Arc<AppState<P>>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions::<P>))
        .route("/v1/models", get(models::<P>))
        .with_state(state)
}

/// Validation at config load rejects dangling references, so any miss here
/// is a programmer error, not user input.
pub(crate) fn judges_for_agent<P: Agents + OneShotPrompt>(
    state: &AppState<P>,
    agent_name: &str,
) -> Vec<Arc<Judge>> {
    let snapshot = state.agents.agents();
    let Some(agent) = snapshot.iter().find(|a| a.name == agent_name) else {
        return Vec::new();
    };
    agent
        .judges
        .iter()
        .filter_map(|name| state.judges.get(name).cloned())
        .collect()
}

/// Emit an `llm_call` tracing span carrying the provider, model, token
/// usage, and computed USD cost for the turn. The span is opened and
/// immediately closed — there's no body to instrument, just a record for
/// the telemetry layer's `on_close` hook to mirror into the `events`
/// table. Pricing misses (model not in the vendored `LiteLLM` table) leave
/// `cost_usd` empty rather than failing the request.
pub(crate) fn record_llm_call<P: Agents + OneShotPrompt>(
    state: &AppState<P>,
    agent_name: &str,
    usage: providers::Usage,
    turn_span: &Span,
) {
    let snapshot = state.agents.agents();
    let Some(agent) = snapshot.iter().find(|a| a.name == agent_name) else {
        return;
    };
    let cost = providers::cost_for(agent.provider, &agent.model, &usage);
    let usage_json = serde_json::to_string(&usage).unwrap_or_default();
    let cost_str = cost.map(|c| format!("{:.6}", c.usd)).unwrap_or_default();
    turn_span.in_scope(|| {
        let _span = info_span!(
            "llm_call",
            cost_usd = %cost_str,
            model = %agent.model,
            provider = %agent.provider,
            usage = %usage_json,
        )
        .entered();
    });
}

async fn chat_completions<P: Agents + OneShotPrompt + 'static>(
    State(state): State<Arc<AppState<P>>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    let mut prepared = prepare_request(&state, &request).await?;
    let routing = resolve_routing(&state, &request, &prepared).await;

    if request.is_streaming() {
        return Ok(stream_response(state, request, prepared, routing)
            .await?
            .into_response());
    }

    let messages = std::mem::take(&mut prepared.messages);
    let completion = state
        .agents
        .complete(&routing.agent_name, messages, prepared.user_id)
        .instrument(routing.turn_span.clone())
        .await?;
    finalize_non_streaming(&state, &prepared, &routing, &completion).await?;

    let usage = proxy::Usage::new(
        completion.usage.input_tokens,
        completion.usage.output_tokens,
        completion.usage.total_tokens,
    );
    Ok(Json(request.response_with(completion.text, usage)).into_response())
}

/// Per-request derived state from experiment routing: which agent to call,
/// the telemetry span, and (if applicable) shadow inputs to spawn.
struct Routing {
    agent_name: String,
    assistant_message_id: MessageId,
    shadow_inputs: Option<(experiments::ExperimentConfig, Vec<agents::Message>)>,
    turn_id: TurnId,
    turn_span: Span,
}

async fn resolve_routing<P: Agents + OneShotPrompt>(
    state: &Arc<AppState<P>>,
    request: &ChatCompletionRequest,
    prepared: &PreparedRequest,
) -> Routing {
    let bandit_scores = match state.experiments.bandit_query(&request.model) {
        Some((judge, criterion, since)) => state
            .judge_store
            .mean_scores_by_agent(&judge, &criterion, since)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let resolved =
        state
            .experiments
            .resolve_with_scores(&request.model, prepared.user_id, &bandit_scores);
    let agent_name = resolved.agent.clone().into_owned();
    let experiment_name = resolved.experiment.map(str::to_owned);

    // Reuse the assistant message UUID as the telemetry turn correlation id
    // so the stored message and its event tree share one identifier.
    let assistant_message_id = MessageId::new();
    let turn_id = TurnId(assistant_message_id.0);
    let turn_span = build_turn_span(&agent_name, experiment_name.as_deref(), turn_id, prepared);

    // Clone inputs for shadow variants so they run against the same context
    // the primary consumed. No-op for non-shadow strategies.
    let shadow_inputs = state
        .experiments
        .get(&request.model)
        .filter(|exp| matches!(exp.strategy, Strategy::Shadow))
        .map(|exp| (exp.clone(), prepared.messages.clone()));

    Routing {
        agent_name,
        assistant_message_id,
        shadow_inputs,
        turn_id,
        turn_span,
    }
}

fn build_turn_span(
    agent_name: &str,
    experiment_name: Option<&str>,
    turn_id: TurnId,
    prepared: &PreparedRequest,
) -> Span {
    if let Some(experiment) = experiment_name {
        info_span!(
            "turn",
            agent = %agent_name,
            experiment = %experiment,
            turn_id = %turn_id.0,
            user_id = %prepared.user_id.0,
            user_message = %prepared.user_message,
        )
    } else {
        info_span!(
            "turn",
            agent = %agent_name,
            turn_id = %turn_id.0,
            user_id = %prepared.user_id.0,
            user_message = %prepared.user_message,
        )
    }
}

async fn stream_response<P: Agents + OneShotPrompt + 'static>(
    state: Arc<AppState<P>>,
    request: ChatCompletionRequest,
    prepared: PreparedRequest,
    routing: Routing,
) -> Result<
    axum::response::sse::Sse<
        impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    ApiError,
> {
    if let Some((experiment, messages)) = routing.shadow_inputs {
        spawn_shadow_runs(
            &state,
            &experiment,
            routing.turn_id,
            prepared.user_id,
            &prepared.user_message,
            &messages,
        );
    }
    let inner = state
        .agents
        .complete_streaming(&routing.agent_name, prepared.messages, prepared.user_id)
        .instrument(routing.turn_span.clone())
        .await?;
    Ok(sse_response(StreamContext {
        agent_name: routing.agent_name,
        assistant_message_id: routing.assistant_message_id,
        include_usage: request.include_usage(),
        inner,
        model: request.model.clone(),
        state: Arc::clone(&state),
        tracker_key: prepared.tracker_key,
        turn_span: routing.turn_span,
        user_id: prepared.user_id,
        user_message: prepared.user_message,
    }))
}

async fn finalize_non_streaming<P: Agents + OneShotPrompt + 'static>(
    state: &Arc<AppState<P>>,
    prepared: &PreparedRequest,
    routing: &Routing,
    completion: &agents::Completion,
) -> Result<(), ApiError> {
    if let Some((experiment, messages)) = routing.shadow_inputs.as_ref() {
        spawn_shadow_runs(
            state,
            experiment,
            routing.turn_id,
            prepared.user_id,
            &prepared.user_message,
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
    record_llm_call(
        state,
        &routing.agent_name,
        completion.usage,
        &routing.turn_span,
    );

    let um = state.memory.for_user(prepared.user_id);
    um.append_message(MemRole::User, prepared.user_message.clone())
        .await?;
    um.append_message_with_id(
        MemRole::Assistant,
        completion.text.clone(),
        routing.assistant_message_id,
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

    let judges = judges_for_agent(state, &routing.agent_name);
    spawn_score(
        judges,
        Arc::clone(&state.judge_store),
        Arc::clone(&state.agents),
        judges::ScoredExchange {
            agent_name: routing.agent_name.clone(),
            assistant_message: completion.text.clone(),
            message_id: routing.assistant_message_id,
            user_id: prepared.user_id,
            user_message: prepared.user_message.clone(),
        },
    );
    Ok(())
}

async fn models<P: Agents + OneShotPrompt>(
    State(state): State<Arc<AppState<P>>>,
) -> Json<serde_json::Value> {
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

async fn prepare_request<P: Agents + OneShotPrompt>(
    state: &Arc<AppState<P>>,
    request: &ChatCompletionRequest,
) -> Result<PreparedRequest, ApiError> {
    let user_id = match state.users {
        Users::Shared => UserId::from_string(SHARED_USER_ID),
        Users::PerRequest => request.user_id().ok_or_else(|| {
            ApiError::BadRequest(
                "this Coulisse instance runs in `users: per-request` mode and requires every \
                 request to identify its user. Set the `safety_identifier` field (preferred) or \
                 the deprecated `user` field on the OpenAI request body. To run a single-user / \
                 trial deployment instead, switch the server to `users: shared` in coulisse.yaml \
                 — note that all requests will then share the same memory."
                    .into(),
            )
        })?,
    };
    let limits = RequestLimits::from_metadata(&request.metadata)?;
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
    use std::fmt::Write as _;
    let mut out = String::from("Known about the user:\n");
    for m in memories {
        let tag = match m.kind {
            MemoryKind::Fact => "fact",
            MemoryKind::Preference => "preference",
        };
        let _ = writeln!(out, "- [{tag}] {}", m.content);
    }
    out
}
