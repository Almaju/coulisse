//! Smoke-test runner. The smoke crate owns config and storage; this
//! module is the request-flow spec for a synthetic run. It mirrors the
//! main chat handler's shape: resolve the experiment variant, drive the
//! agent, persist the turn, and spawn the judge fan-out — but with a
//! persona LLM standing in for a human user.
//!
//! Implements [`smoke::RunDispatcher`] so the smoke admin's "Run now"
//! button can launch runs without taking a hard dep on `agents` or
//! `judges`.
//!
//! Smoke runs never write to the user's memory or rate-limit windows.
//! Each repetition uses a fresh synthetic `UserId` (a v4 UUID) so
//! sticky-by-user routing samples experiment variants naturally across
//! repetitions.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use agents::{Agents, Message as AgentMessage, Role as AgentRole};
use coulisse_core::{MessageId, OneShotPrompt, UserId};
use judges::spawn_score;
use providers::ProviderKind;
use smoke::{
    DispatchError, PersonaConfig, RunDispatcher, RunId, RunStatus, SmokeList, SmokeStore,
    SmokeTestConfig,
};
use tracing::{Instrument, info_span};

use crate::server::{AppState, judges_for_agent};

/// Wires the smoke admin router to the live agent + judge runtime in
/// `AppState`. One instance per process; cli builds it once and clones
/// the `Arc` into the smoke router.
pub struct SmokeRunner<P: Agents + OneShotPrompt + 'static> {
    pub configs: SmokeList,
    pub state: Arc<AppState<P>>,
    pub store: Arc<SmokeStore>,
}

impl<P: Agents + OneShotPrompt + 'static> RunDispatcher for SmokeRunner<P> {
    fn dispatch<'a>(
        &'a self,
        test_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RunId>, DispatchError>> + Send + 'a>> {
        Box::pin(async move {
            let config = self
                .configs
                .load()
                .iter()
                .find(|c| c.name == test_name)
                .cloned()
                .ok_or_else(|| DispatchError::NotFound(test_name.to_string()))?;

            let mut ids = Vec::with_capacity(config.repetitions as usize);
            for _ in 0..config.repetitions.max(1) {
                let id = self
                    .store
                    .start_run(&config.name)
                    .await
                    .map_err(|e| DispatchError::other(e.to_string()))?;
                ids.push(id);
                let state = Arc::clone(&self.state);
                let store = Arc::clone(&self.store);
                let cfg = config.clone();
                tokio::spawn(async move {
                    if let Err(err) = run_once(state, store.clone(), cfg, id).await {
                        let msg = err.to_string();
                        tracing::warn!(run = %id.0, error = %msg, "smoke run failed");
                        if let Err(store_err) =
                            store.finish_run(id, RunStatus::Failed, Some(&msg)).await
                        {
                            tracing::warn!(error = %store_err, "failed to persist smoke failure");
                        }
                    }
                });
            }
            Ok(ids)
        })
    }
}

/// One synthetic conversation. Errors bubble up so the spawn wrapper
/// can mark the run failed; success paths mark it completed inline.
async fn run_once<P: Agents + OneShotPrompt + 'static>(
    state: Arc<AppState<P>>,
    store: Arc<SmokeStore>,
    config: SmokeTestConfig,
    run_id: RunId,
) -> Result<(), RunError> {
    let synthetic_user = UserId::new();
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut resolved_recorded = false;

    for turn_index in 0..config.max_turns {
        let persona_text = if turn_index == 0
            && let Some(initial) = config.initial_message.as_ref()
        {
            initial.clone()
        } else {
            persona_turn(&state, &config.persona, &messages).await?
        };
        store
            .record_persona_turn(run_id, turn_index, &persona_text)
            .await
            .map_err(|e| RunError::Store(e.to_string()))?;
        messages.push(AgentMessage {
            content: persona_text.clone(),
            role: AgentRole::User,
        });
        if matches_marker(&persona_text, config.stop_marker.as_deref()) {
            break;
        }

        let assistant_message_id = MessageId::new();
        let resolved = resolve_target(&state, &config.target, synthetic_user).await;
        if !resolved_recorded {
            store
                .set_resolution(run_id, &resolved.agent, resolved.experiment.as_deref())
                .await
                .map_err(|e| RunError::Store(e.to_string()))?;
            resolved_recorded = true;
        }
        let span = info_span!(
            "smoke_turn",
            agent = %resolved.agent,
            run_id = %run_id.0,
            turn_id = %assistant_message_id.0,
            user_id = %synthetic_user.0,
        );
        let completion = state
            .agents
            .complete(&resolved.agent, messages.clone(), synthetic_user)
            .instrument(span)
            .await
            .map_err(|e| RunError::Agent(e.to_string()))?;
        store
            .record_assistant_turn(run_id, turn_index, assistant_message_id, &completion.text)
            .await
            .map_err(|e| RunError::Store(e.to_string()))?;
        messages.push(AgentMessage {
            content: completion.text.clone(),
            role: AgentRole::Assistant,
        });

        let judges = judges_for_agent(&state, &resolved.agent);
        spawn_score(
            judges,
            Arc::clone(&state.judge_store),
            Arc::clone(&state.agents),
            judges::ScoredExchange {
                agent_name: resolved.agent.clone(),
                assistant_message: completion.text.clone(),
                message_id: assistant_message_id,
                user_id: synthetic_user,
                user_message: persona_text.clone(),
            },
        );

        if matches_marker(&completion.text, config.stop_marker.as_deref()) {
            break;
        }
    }

    store
        .finish_run(run_id, RunStatus::Completed, None)
        .await
        .map_err(|e| RunError::Store(e.to_string()))?;
    Ok(())
}

struct ResolvedTarget {
    agent: String,
    experiment: Option<String>,
}

async fn resolve_target<P: Agents + OneShotPrompt>(
    state: &AppState<P>,
    target: &str,
    user_id: UserId,
) -> ResolvedTarget {
    let bandit_scores = match state.experiments.bandit_query(target) {
        Some((judge, criterion, since)) => state
            .judge_store
            .mean_scores_by_agent(&judge, &criterion, since)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let resolved = state
        .experiments
        .resolve_with_scores(target, user_id, &bandit_scores);
    ResolvedTarget {
        agent: resolved.agent.into_owned(),
        experiment: resolved.experiment.map(std::borrow::ToOwned::to_owned),
    }
}

/// Persona turn: ask the persona's model to produce the next user
/// utterance given the conversation so far. Conversation roles are
/// flipped (assistant turns become "user" inputs, persona's previous
/// outputs become "assistant" inputs) so the model speaks *as* the
/// user. Uses the unconfigured `prompt_with` path so the persona has
/// no MCP tools, no subagents, no preamble merging — just its own
/// system prompt.
async fn persona_turn<P: Agents + OneShotPrompt>(
    state: &AppState<P>,
    persona: &PersonaConfig,
    history: &[AgentMessage],
) -> Result<String, RunError> {
    let provider = ProviderKind::parse(&persona.provider).ok_or_else(|| {
        RunError::Persona(format!("unknown persona provider '{}'", persona.provider))
    })?;
    let flipped: Vec<AgentMessage> = history
        .iter()
        .map(|m| AgentMessage {
            content: m.content.clone(),
            role: match m.role {
                AgentRole::Assistant => AgentRole::User,
                AgentRole::User => AgentRole::Assistant,
                AgentRole::System => AgentRole::System,
            },
        })
        .collect();
    let messages = if flipped.is_empty() {
        vec![AgentMessage {
            content: "Begin the conversation. Send your first message to the assistant."
                .to_string(),
            role: AgentRole::User,
        }]
    } else {
        flipped
    };
    let completion = state
        .agents
        .prompt_with(provider, &persona.model, &persona.preamble, messages)
        .await
        .map_err(|e| RunError::Persona(e.to_string()))?;
    Ok(completion.text)
}

fn matches_marker(text: &str, marker: Option<&str>) -> bool {
    match marker {
        Some(m) if !m.is_empty() => text.contains(m),
        _ => false,
    }
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("agent: {0}")]
    Agent(String),
    #[error("persona: {0}")]
    Persona(String),
    #[error("store: {0}")]
    Store(String),
}
