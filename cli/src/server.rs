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
use judges::{Judge, Judges};
use limits::{RequestLimits, Tracker};
use memory::{Exchange, Extractor, Memory, MemoryKind, MessageId, Role as MemRole, Store, UserId};
use providers::PricingTable;
use telemetry::TurnId;
use tracing::{Instrument, Span, info_span};

use proxy::Message as ChatMessage;
use proxy::{ChatCompletionRequest, ResponseFormat, TokenCounts};

use crate::error::ApiError;
use crate::shadow::ShadowRun;
use crate::stream::StreamContext;

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
    /// How a request's user identity is resolved.
    pub identity: Identity,
    /// Persistent score store owned by the judge crate. Reads (for
    /// bandit aggregates) and writes (from background scoring tasks)
    /// both go here.
    pub judge_store: Arc<Judges>,
    /// All judges configured in YAML, keyed by name. Agents opt in by listing
    /// judge names on themselves — the per-request handler looks up which of
    /// these apply to the agent being called.
    pub judges: Arc<HashMap<String, Arc<Judge>>>,
    pub memory: Arc<Store>,
    /// The vendored `LiteLLM` price table, parsed once at boot so the first
    /// chat completion doesn't pay for ~9k JSON entries on the request path.
    pub pricing: Arc<PricingTable>,
    /// Self-issued API-token store. Always present (the studio token page is
    /// always mounted), but only exercised when a request carries an
    /// authenticated token id — i.e. when `auth.proxy.tokens` is configured.
    /// Drives per-token budget enforcement and spend recording.
    pub tokens: Arc<TokenStore>,
    pub tracker: Tracker,
}

/// How a request's user identity is resolved. `FromRequest` (default)
/// trusts the body's `safety_identifier`; `FromCredential` binds it to
/// the authenticated principal carried in request extensions.
#[derive(Clone, Copy, Debug)]
pub struct Identity {
    /// Fallback user id applied to requests that don't supply their own.
    /// `None` means such requests are rejected (multi-tenant posture).
    pub default_user_id: Option<UserId>,
    pub mode: IdentityMode,
}

impl Identity {
    /// Resolve which user a request belongs to. In `FromRequest` mode the
    /// identity is whatever the body claims (`safety_identifier`), falling
    /// back to `default_user_id`. In `FromCredential` mode it is the
    /// authenticated principal, and a body that claims a *different*
    /// identifier is rejected so a credentialed client cannot reach into
    /// another user's data.
    fn resolve(
        &self,
        request: &ChatCompletionRequest,
        principal: Option<&AuthenticatedPrincipal>,
    ) -> Result<UserId, ApiError> {
        match self.mode {
            IdentityMode::FromRequest => request
                .user_id()
                .or(self.default_user_id)
                .ok_or_else(|| {
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
}

/// How many times a structured-output reply may be re-prompted before the
/// request fails. Each retry feeds the model its own invalid reply plus the
/// exact validation error, so two attempts clears all but pathological cases
/// without burning unbounded tokens on a model that simply can't comply.
const MAX_FORMAT_REPAIRS: usize = 2;

impl<P: Agents + OneShotPrompt + 'static> AppState<P> {
    /// Validation at config load rejects dangling references, so any miss here
    /// is a programmer error, not user input.
    pub(crate) fn judges_for_agent(&self, agent_name: &str) -> Vec<Arc<Judge>> {
        let snapshot = self.agents.agents();
        let Some(agent) = snapshot.iter().find(|a| a.name == agent_name) else {
            return Vec::new();
        };
        agent
            .judges
            .iter()
            .filter_map(|name| self.judges.get(name).cloned())
            .collect()
    }

    /// Emit an `llm_call` tracing span carrying the provider, model, token
    /// usage, and computed USD cost for the turn. The span is opened and
    /// immediately closed — there's no body to instrument, just a record for
    /// the telemetry layer's `on_close` hook to mirror into the `events`
    /// table. Pricing misses (model not in the vendored `LiteLLM` table) leave
    /// `cost_usd` empty rather than failing the request.
    pub(crate) fn record_llm_call(
        &self,
        agent_name: &str,
        usage: providers::Usage,
        turn_span: &Span,
    ) {
        let snapshot = self.agents.agents();
        let Some(agent) = snapshot.iter().find(|a| a.name == agent_name) else {
            return;
        };
        let cost = self.pricing.cost_for(agent.provider, &agent.model, &usage);
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
    pub(crate) async fn record_token_spend(
        &self,
        token_id: Option<TokenId>,
        agent_name: &str,
        usage: providers::Usage,
    ) {
        let Some(token_id) = token_id else {
            return;
        };
        let snapshot = self.agents.agents();
        let Some(agent) = snapshot.iter().find(|a| a.name == agent_name) else {
            return;
        };
        let Some(cost) = self.pricing.cost_for(agent.provider, &agent.model, &usage) else {
            return;
        };
        // A turn's USD cost times 1e6 is microdollars — always well within i64.
        #[allow(clippy::cast_possible_truncation)]
        let micro_usd = (cost.usd * 1_000_000.0).round() as i64;
        if let Err(err) = self.tokens.record_spend(token_id, micro_usd).await {
            tracing::warn!(error = %err, "token spend record failed");
        }
    }

    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat_completions::<P>))
            .route("/v1/models", get(models::<P>))
            .with_state(self)
    }

    /// Run the agent and, when a JSON `response_format` was requested, enforce it:
    /// validate the reply, and on failure re-prompt with the validation error up
    /// to `MAX_FORMAT_REPAIRS` times. Returns the cleaned JSON as the reply text
    /// (fences and stray prose stripped) and the cumulative usage across every
    /// attempt. A reply that never validates surfaces as a `ResponseFormat` error.
    async fn complete_validated(
        &self,
        routing: &Routing,
        mut messages: Vec<agents::Message>,
        user_id: UserId,
        format: Option<&ResponseFormat>,
    ) -> Result<agents::Completion, ApiError> {
        let mut completion = self
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
                    completion = self
                        .agents
                        .complete(&routing.agent_name, messages.clone(), user_id)
                        .instrument(routing.turn_span.clone())
                        .await?;
                    usage = usage.merged(completion.usage);
                }
            }
        }
    }

    async fn finalize_non_streaming(
        self: &Arc<Self>,
        prepared: &PreparedRequest,
        routing: &Routing,
        completion: &agents::Completion,
    ) -> Result<(), ApiError> {
        if let Some((experiment, messages)) = routing.shadow_inputs.as_ref() {
            self.spawn_shadow_runs(experiment, prepared, routing.turn_id, messages);
        }
        if let Err(err) = self
            .tracker
            .record(&prepared.tracker_key, completion.usage.total_tokens)
            .await
        {
            tracing::warn!(error = %err, "rate limit record failed");
        }
        self.record_llm_call(&routing.agent_name, completion.usage, &routing.turn_span);
        self.record_token_spend(prepared.token_id, &routing.agent_name, completion.usage)
            .await;

        let um = self.memory.for_user(prepared.user_id);
        um.append_message(MemRole::User, prepared.user_message.clone())
            .await?;
        um.append_message_with_id(
            MemRole::Assistant,
            completion.text.clone(),
            routing.assistant_message_id,
        )
        .await?;

        if let Some(extractor) = self.extractor.as_ref() {
            extractor.spawn(
                Arc::clone(&self.memory),
                prepared.user_id,
                Exchange {
                    assistant_message: completion.text.clone(),
                    user_message: prepared.user_message.clone(),
                },
            );
        }

        let judges = self.judges_for_agent(&routing.agent_name);
        self.judge_store.spawn_score(
            Arc::clone(&self.agents),
            judges::ScoredExchange {
                agent_name: routing.agent_name.clone(),
                assistant_message: completion.text.clone(),
                message_id: routing.assistant_message_id,
                user_id: prepared.user_id,
                user_message: prepared.user_message.clone(),
            },
            judges,
        );
        Ok(())
    }

    async fn prepare_request(
        &self,
        request: &ChatCompletionRequest,
        principal: Option<&AuthenticatedPrincipal>,
        token_id: Option<TokenId>,
    ) -> Result<PreparedRequest, ApiError> {
        let user_id = self.identity.resolve(request, principal)?;
        let limits = RequestLimits::from_metadata(&request.metadata)?;
        let language = request.language()?;
        let tracker_key = user_id.0.to_string();
        self.tracker.check(&tracker_key, limits).await?;
        // Budget gate sits beside the rate-limit check: reject before spending
        // any provider tokens when this credential has hit its cap. Only requests
        // that authenticated with a token carry a `token_id`.
        if let Some(token_id) = token_id {
            self.tokens.check_budget(token_id).await?;
        }

        let last_user: &ChatMessage = request
            .last_user_message()
            .ok_or_else(|| ApiError::BadRequest("no user message to respond to".into()))?;
        let user_message = last_user.content_or_empty().clone();
        let um = self.memory.for_user(user_id);
        let budget = self.memory.config().context_budget;
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
                content: sys.content_or_empty().clone(),
                role: agents::Role::System,
            });
        }
        if let Some(format) = request.response_format.as_ref() {
            messages.extend(PreparedRequest::structured_output_instruction(format)?);
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

    async fn resolve_routing(
        &self,
        request: &ChatCompletionRequest,
        prepared: &PreparedRequest,
    ) -> Routing {
        let bandit_scores = match self.experiments.bandit_query(&request.model) {
            None => Vec::new(),
            Some(query) => self
                .judge_store
                .mean_scores_by_agent(query)
                .await
                .unwrap_or_default(),
        };
        let resolved =
            self.experiments
                .resolve_with_scores(&request.model, prepared.user_id, &bandit_scores);
        let agent_name = resolved.agent.clone().into_owned();
        let experiment_name = resolved.experiment.map(str::to_owned);

        // WHY: reuse the assistant message UUID as the telemetry turn
        // correlation id so the stored message and its event tree share one
        // identifier.
        let assistant_message_id = MessageId::new();
        let turn_id = TurnId(assistant_message_id.0);
        let turn_span = prepared.turn_span(&agent_name, experiment_name.as_deref(), turn_id);

        // WHY: clone inputs for shadow variants so they run against the same
        // context the primary consumed. No-op for non-shadow strategies.
        let shadow_inputs = self
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

    /// Spawn a background task per non-primary variant of a shadow
    /// experiment. Each task runs the variant against the same prepared
    /// context the primary saw, scores its output, and persists the
    /// scores. Failures are logged and swallowed — shadow is best-effort.
    fn spawn_shadow_runs(
        self: &Arc<Self>,
        experiment: &experiments::ExperimentConfig,
        prepared: &PreparedRequest,
        parent_turn: TurnId,
        messages: &[agents::Message],
    ) {
        if !self
            .experiments
            .shadow_should_sample(experiment, prepared.user_id)
        {
            return;
        }
        let variants: Vec<String> = self
            .experiments
            .shadow_variants(experiment)
            .map(|v| v.agent.clone())
            .collect();
        for agent_name in variants {
            ShadowRun {
                agent_name,
                messages: messages.to_vec(),
                parent_turn,
                user_id: prepared.user_id,
                user_message: prepared.user_message.clone(),
            }
            .spawn(Arc::clone(self));
        }
    }

    async fn stream_response(
        self: Arc<Self>,
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
            self.spawn_shadow_runs(&experiment, &prepared, routing.turn_id, &messages);
        }
        let inner = self
            .agents
            .complete_streaming(&routing.agent_name, prepared.messages, prepared.user_id)
            .instrument(routing.turn_span.clone())
            .await?;
        Ok(StreamContext {
            agent_name: routing.agent_name,
            assistant_message_id: routing.assistant_message_id,
            include_usage: request.include_usage(),
            inner,
            model: request.model.clone(),
            response_format: request.response_format.clone(),
            state: Arc::clone(&self),
            token_id: prepared.token_id,
            tracker_key: prepared.tracker_key,
            turn_span: routing.turn_span,
            user_id: prepared.user_id,
            user_message: prepared.user_message,
        }
        .into_sse())
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
    let mut prepared = state
        .prepare_request(&request, principal.as_ref(), token_id)
        .await?;
    let routing = state.resolve_routing(&request, &prepared).await;

    if request.is_streaming() {
        return Ok(state
            .stream_response(request, prepared, routing)
            .await?
            .into_response());
    }

    let messages = std::mem::take(&mut prepared.messages);
    let completion = state
        .complete_validated(
            &routing,
            messages,
            prepared.user_id,
            request.response_format.as_ref(),
        )
        .await?;
    state
        .finalize_non_streaming(&prepared, &routing, &completion)
        .await?;

    let usage = proxy::Usage::from(TokenCounts {
        completion: completion.usage.output_tokens,
        prompt: completion.usage.input_tokens,
        total: completion.usage.total_tokens,
    });
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

impl PreparedRequest {
    /// Structured output is enforced uniformly for every provider: reject a
    /// malformed schema up front, then inject the shape instruction so even
    /// models with no native structured-output mode emit conforming JSON.
    /// The reply is validated (and repaired) after the call — see
    /// `complete_validated` and the streaming branch.
    fn structured_output_instruction(
        format: &ResponseFormat,
    ) -> Result<Option<agents::Message>, ApiError> {
        format.check_schema()?;
        Ok(format.instruction().map(|content| agents::Message {
            content,
            role: agents::Role::System,
        }))
    }

    /// The root `turn` span every event of this request nests under.
    fn turn_span(&self, agent_name: &str, experiment_name: Option<&str>, turn_id: TurnId) -> Span {
        if let Some(experiment) = experiment_name {
            info_span!(
                "turn",
                agent = %agent_name,
                experiment = %experiment,
                turn_id = %turn_id.0,
                user_id = %self.user_id.0,
                user_message = %self.user_message,
            )
        } else {
            info_span!(
                "turn",
                agent = %agent_name,
                turn_id = %turn_id.0,
                user_id = %self.user_id.0,
                user_message = %self.user_message,
            )
        }
    }
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

    fn identity(mode: IdentityMode, default_user_id: Option<UserId>) -> Identity {
        Identity {
            default_user_id,
            mode,
        }
    }

    #[test]
    fn from_request_uses_body_identifier() {
        let resolved = identity(IdentityMode::FromRequest, None)
            .resolve(&request(Some("alice")), None)
            .expect("body identifier accepted");
        assert_eq!(resolved, UserId::from_string("alice"));
    }

    #[test]
    fn from_request_falls_back_to_default() {
        let resolved = identity(IdentityMode::FromRequest, Some(UserId::from_string("main")))
            .resolve(&request(None), None)
            .expect("default applied");
        assert_eq!(resolved, UserId::from_string("main"));
    }

    #[test]
    fn from_request_without_identifier_or_default_is_rejected() {
        let err = identity(IdentityMode::FromRequest, None)
            .resolve(&request(None), None)
            .expect_err("missing identifier rejected");
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn from_credential_uses_principal_ignoring_body() {
        let principal = AuthenticatedPrincipal("alice".into());
        let resolved = identity(IdentityMode::FromCredential, None)
            .resolve(&request(None), Some(&principal))
            .expect("principal accepted");
        assert_eq!(resolved, UserId::from_string("alice"));
    }

    #[test]
    fn from_credential_allows_matching_body_identifier() {
        let principal = AuthenticatedPrincipal("alice".into());
        let resolved = identity(IdentityMode::FromCredential, None)
            .resolve(&request(Some("alice")), Some(&principal))
            .expect("matching identifier accepted");
        assert_eq!(resolved, UserId::from_string("alice"));
    }

    #[test]
    fn from_credential_rejects_mismatched_body_identifier() {
        let principal = AuthenticatedPrincipal("alice".into());
        let err = identity(IdentityMode::FromCredential, None)
            .resolve(&request(Some("bob")), Some(&principal))
            .expect_err("spoofed identifier rejected");
        assert!(matches!(err, ApiError::Forbidden(_)));
    }

    #[test]
    fn from_credential_without_principal_is_forbidden() {
        let err = identity(IdentityMode::FromCredential, None)
            .resolve(&request(None), None)
            .expect_err("missing principal rejected");
        assert!(matches!(err, ApiError::Forbidden(_)));
    }
}
