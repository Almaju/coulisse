use std::collections::HashMap;
use std::sync::Arc;

use agents::Agents;
use auth::{AuthenticatedPrincipal, AuthenticatedToken, IdentityMode, TokenId, TokenStore};
use axum::Json;
use axum::Router;
use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coulisse_core::OneShotPrompt;
use experiments::{ExperimentRouter, Strategy};
use judges::{Judge, Judges, spawn_score};
use limits::{RequestLimits, Tracker};
use memory::{Extractor, Memory, MemoryKind, MessageId, Role as MemRole, Store, UserId};
use telemetry::TurnId;
use tracing::{Instrument, Span, info_span};

use proxy::Message as ChatMessage;
use proxy::{ChatCompletionRequest, ResponseFormat};

use crate::error::ApiError;
use crate::shadow::spawn_shadow_runs;
use crate::stream::{StreamContext, sse_response};

/// Shared state for the OpenAI-compatible proxy. Held in an `Arc` so axum
/// handlers can cheaply clone the reference.
pub struct AppState<P: Agents + OneShotPrompt> {
    pub agents: Arc<P>,
    /// Fallback user id applied to requests that don't supply their own.
    /// `None` means such requests are rejected (multi-tenant posture).
    pub default_user_id: Option<UserId>,
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
    /// How a request's user identity is resolved. `FromRequest` (default)
    /// trusts the body's `safety_identifier`; `FromCredential` binds it to
    /// the authenticated principal carried in request extensions.
    pub proxy_identity: IdentityMode,
    /// Self-issued API-token store. Always present (the studio token page is
    /// always mounted), but only exercised when a request carries an
    /// authenticated token id — i.e. when `auth.proxy.tokens` is configured.
    /// Drives per-token budget enforcement and spend recording.
    pub tokens: Arc<TokenStore>,
    pub tracker: Tracker,
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

/// Charge a turn's USD cost to the API token that authorized it. No-op when
/// token auth is off, when the request carried no token, or when the model
/// isn't in the pricing table. Computes the same cost `record_llm_call`
/// logs; the redundant `cost_for` lookup is a hashmap hit, off any tight
/// loop.
pub(crate) async fn record_token_spend<P: Agents + OneShotPrompt>(
    state: &AppState<P>,
    token_id: Option<TokenId>,
    agent_name: &str,
    usage: providers::Usage,
) {
    let Some(token_id) = token_id else {
        return;
    };
    let snapshot = state.agents.agents();
    let Some(agent) = snapshot.iter().find(|a| a.name == agent_name) else {
        return;
    };
    let Some(cost) = providers::cost_for(agent.provider, &agent.model, &usage) else {
        return;
    };
    // A turn's USD cost times 1e6 is microdollars — always well within i64.
    #[allow(clippy::cast_possible_truncation)]
    let micro_usd = (cost.usd * 1_000_000.0).round() as i64;
    if let Err(err) = state.tokens.record_spend(token_id, micro_usd).await {
        eprintln!("token spend record failed: {err}");
    }
}

async fn chat_completions<P: Agents + OneShotPrompt + 'static>(
    State(state): State<Arc<AppState<P>>>,
    principal: Option<Extension<AuthenticatedPrincipal>>,
    token: Option<Extension<AuthenticatedToken>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    let principal = principal.map(|Extension(p)| p);
    let token_id = token.map(|Extension(t)| t.0);
    let mut prepared = prepare_request(&state, &request, principal.as_ref(), token_id).await?;
    let routing = resolve_routing(&state, &request, &prepared).await;

    if request.is_streaming() {
        return Ok(stream_response(state, request, prepared, routing)
            .await?
            .into_response());
    }

    let messages = std::mem::take(&mut prepared.messages);
    let completion = complete_validated(
        &state,
        &routing,
        messages,
        prepared.user_id,
        request.response_format.as_ref(),
    )
    .await?;
    finalize_non_streaming(&state, &prepared, &routing, &completion).await?;

    let usage = proxy::Usage::new(
        completion.usage.input_tokens,
        completion.usage.output_tokens,
        completion.usage.total_tokens,
    );
    Ok(Json(request.response_with(completion.text, usage)).into_response())
}

/// How many times a structured-output reply may be re-prompted before the
/// request fails. Each retry feeds the model its own invalid reply plus the
/// exact validation error, so two attempts clears all but pathological cases
/// without burning unbounded tokens on a model that simply can't comply.
const MAX_FORMAT_REPAIRS: usize = 2;

/// Run the agent and, when a JSON `response_format` was requested, enforce it:
/// validate the reply, and on failure re-prompt with the validation error up
/// to `MAX_FORMAT_REPAIRS` times. Returns the cleaned JSON as the reply text
/// (fences and stray prose stripped) and the cumulative usage across every
/// attempt. A reply that never validates surfaces as a `ResponseFormat` error.
async fn complete_validated<P: Agents + OneShotPrompt + 'static>(
    state: &Arc<AppState<P>>,
    routing: &Routing,
    mut messages: Vec<agents::Message>,
    user_id: UserId,
    format: Option<&ResponseFormat>,
) -> Result<agents::Completion, ApiError> {
    let mut completion = state
        .agents
        .complete(&routing.agent_name, messages.clone(), user_id)
        .instrument(routing.turn_span.clone())
        .await?;

    let Some(format) = format.filter(|f| f.requires_json()) else {
        return Ok(completion);
    };

    let mut usage = completion.usage;
    let mut attempts = 0;
    loop {
        match format.validate(&completion.text) {
            Ok(json) => {
                completion.text = json;
                completion.usage = usage;
                return Ok(completion);
            }
            Err(err) if attempts >= MAX_FORMAT_REPAIRS => {
                return Err(ApiError::ResponseFormat(err));
            }
            Err(err) => {
                attempts += 1;
                messages.push(agents::Message {
                    content: std::mem::take(&mut completion.text),
                    role: agents::Role::Assistant,
                });
                messages.push(agents::Message {
                    content: format.repair_instruction(&err),
                    role: agents::Role::User,
                });
                completion = state
                    .agents
                    .complete(&routing.agent_name, messages.clone(), user_id)
                    .instrument(routing.turn_span.clone())
                    .await?;
                usage = usage.merged(completion.usage);
            }
        }
    }
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
        None => Vec::new(),
        Some((judge, criterion, since)) => state
            .judge_store
            .mean_scores_by_agent(&judge, &criterion, since)
            .await
            .unwrap_or_default(),
    };
    let resolved =
        state
            .experiments
            .resolve_with_scores(&request.model, prepared.user_id, &bandit_scores);
    let agent_name = resolved.agent.clone().into_owned();
    let experiment_name = resolved.experiment.map(str::to_owned);

    // WHY: reuse the assistant message UUID as the telemetry turn
    // correlation id so the stored message and its event tree share one
    // identifier.
    let assistant_message_id = MessageId::new();
    let turn_id = TurnId(assistant_message_id.0);
    let turn_span = build_turn_span(&agent_name, experiment_name.as_deref(), turn_id, prepared);

    // WHY: clone inputs for shadow variants so they run against the same
    // context the primary consumed. No-op for non-shadow strategies.
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
        response_format: request.response_format.clone(),
        state: Arc::clone(&state),
        token_id: prepared.token_id,
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
    record_token_spend(
        state,
        prepared.token_id,
        &routing.agent_name,
        completion.usage,
    )
    .await;

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
    /// The API token this request authenticated with, when token auth is in
    /// effect. Threaded to the finalize paths so spend is charged to it.
    token_id: Option<TokenId>,
    tracker_key: String,
    user_id: UserId,
    user_message: String,
}

/// Resolve which user a request belongs to. In `FromRequest` mode the
/// identity is whatever the body claims (`safety_identifier`), falling back
/// to `default_user_id`. In `FromCredential` mode it is the authenticated
/// principal, and a body that claims a *different* identifier is rejected so
/// a credentialed client cannot reach into another user's data.
fn resolve_user_id(
    mode: IdentityMode,
    default_user_id: Option<UserId>,
    request: &ChatCompletionRequest,
    principal: Option<&AuthenticatedPrincipal>,
) -> Result<UserId, ApiError> {
    match mode {
        IdentityMode::FromRequest => request.user_id().or(default_user_id).ok_or_else(|| {
            ApiError::BadRequest(
                "missing user identifier: set `safety_identifier` (preferred) or the deprecated `user` field"
                    .into(),
            )
        }),
        IdentityMode::FromCredential => {
            // WHY: `from_credential` is only reachable with proxy auth
            // configured, so a missing principal means the auth layer was
            // bypassed — fail closed rather than fall back to the body.
            let principal = principal.ok_or_else(|| {
                ApiError::Forbidden(
                    "credential-bound identity requires an authenticated request".into(),
                )
            })?;
            if let Some(claimed) = request.user_key()
                && claimed != principal.0
            {
                return Err(ApiError::Forbidden(
                    "safety_identifier does not match the authenticated principal".into(),
                ));
            }
            Ok(UserId::from_string(&principal.0))
        }
    }
}

async fn prepare_request<P: Agents + OneShotPrompt>(
    state: &Arc<AppState<P>>,
    request: &ChatCompletionRequest,
    principal: Option<&AuthenticatedPrincipal>,
    token_id: Option<TokenId>,
) -> Result<PreparedRequest, ApiError> {
    let user_id = resolve_user_id(
        state.proxy_identity,
        state.default_user_id,
        request,
        principal,
    )?;
    let limits = RequestLimits::from_metadata(&request.metadata)?;
    let language = request.language()?;
    let tracker_key = user_id.0.to_string();
    state.tracker.check(&tracker_key, limits).await?;
    // Budget gate sits beside the rate-limit check: reject before spending
    // any provider tokens when this credential has hit its cap. Only requests
    // that authenticated with a token carry a `token_id`.
    if let Some(token_id) = token_id {
        state.tokens.check_budget(token_id).await?;
    }

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
    // Structured output is enforced uniformly for every provider: reject a
    // malformed schema up front, then inject the shape instruction so even
    // models with no native structured-output mode emit conforming JSON.
    // The reply is validated (and repaired) after the call — see
    // `complete_validated` and the streaming branch.
    if let Some(format) = request.response_format.as_ref() {
        format.check_schema()?;
        if let Some(instruction) = format.instruction() {
            messages.push(agents::Message {
                content: instruction,
                role: agents::Role::System,
            });
        }
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
        token_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(safety_identifier: Option<&str>) -> ChatCompletionRequest {
        let mut body = serde_json::json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "model": "assistant",
        });
        if let Some(id) = safety_identifier {
            body["safety_identifier"] = serde_json::json!(id);
        }
        serde_json::from_value(body).expect("valid request")
    }

    #[test]
    fn from_request_uses_body_identifier() {
        let resolved = resolve_user_id(
            IdentityMode::FromRequest,
            None,
            &request(Some("alice")),
            None,
        )
        .expect("body identifier accepted");
        assert_eq!(resolved, UserId::from_string("alice"));
    }

    #[test]
    fn from_request_falls_back_to_default() {
        let resolved = resolve_user_id(
            IdentityMode::FromRequest,
            Some(UserId::from_string("main")),
            &request(None),
            None,
        )
        .expect("default applied");
        assert_eq!(resolved, UserId::from_string("main"));
    }

    #[test]
    fn from_request_without_identifier_or_default_is_rejected() {
        let err = resolve_user_id(IdentityMode::FromRequest, None, &request(None), None)
            .expect_err("missing identifier rejected");
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn from_credential_uses_principal_ignoring_body() {
        let principal = AuthenticatedPrincipal("alice".into());
        let resolved = resolve_user_id(
            IdentityMode::FromCredential,
            None,
            &request(None),
            Some(&principal),
        )
        .expect("principal accepted");
        assert_eq!(resolved, UserId::from_string("alice"));
    }

    #[test]
    fn from_credential_allows_matching_body_identifier() {
        let principal = AuthenticatedPrincipal("alice".into());
        let resolved = resolve_user_id(
            IdentityMode::FromCredential,
            None,
            &request(Some("alice")),
            Some(&principal),
        )
        .expect("matching identifier accepted");
        assert_eq!(resolved, UserId::from_string("alice"));
    }

    #[test]
    fn from_credential_rejects_mismatched_body_identifier() {
        let principal = AuthenticatedPrincipal("alice".into());
        let err = resolve_user_id(
            IdentityMode::FromCredential,
            None,
            &request(Some("bob")),
            Some(&principal),
        )
        .expect_err("spoofed identifier rejected");
        assert!(matches!(err, ApiError::Forbidden(_)));
    }

    #[test]
    fn from_credential_without_principal_is_forbidden() {
        let err = resolve_user_id(IdentityMode::FromCredential, None, &request(None), None)
            .expect_err("missing principal rejected");
        assert!(matches!(err, ApiError::Forbidden(_)));
    }
}
