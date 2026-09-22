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

use std::sync::Arc;

use agents::{Agents, AgentsError, Message as AgentMessage, PromptRequest, Role as AgentRole};
use coulisse_core::{BoxFuture, MessageId, OneShotPrompt, UserId};
use providers::ProviderKind;
use smoke::{
    DispatchError, PersonaConfig, RunDispatcher, RunId, RunStatus, SmokeList, SmokeStore,
    SmokeStoreError, SmokeTestConfig,
};
use tracing::{Instrument, info_span};

use crate::server::AppState;

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
    ) -> BoxFuture<'a, Result<Vec<RunId>, DispatchError>> {
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
                let id = self.store.start_run(&config.name).await?;
                ids.push(id);
                let runner = self.share();
                let cfg = config.clone();
                tokio::spawn(async move {
                    if let Err(err) = runner.run_once(cfg, id).await {
                        let msg = err.to_string();
                        tracing::warn!(run = %id.0, error = %msg, "smoke run failed");
                        if let Err(store_err) = runner
                            .store
                            .finish_run(id, RunStatus::Failed, Some(&msg))
                            .await
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

impl<P: Agents + OneShotPrompt + 'static> SmokeRunner<P> {
    /// Persona turn: ask the persona's model to produce the next user
    /// utterance given the conversation so far. Conversation roles are
    /// flipped (assistant turns become "user" inputs, persona's previous
    /// outputs become "assistant" inputs) so the model speaks *as* the
    /// user. Uses the unconfigured `prompt_with` path so the persona has
    /// no MCP tools, no subagents, no preamble merging — just its own
    /// system prompt.
    async fn persona_turn(
        &self,
        persona: &PersonaConfig,
        history: &[AgentMessage],
    ) -> Result<String, RunError> {
        let provider = ProviderKind::parse(&persona.provider)
            .ok_or_else(|| RunError::UnknownPersonaProvider(persona.provider.clone()))?;
        let flipped: Vec<AgentMessage> = history
            .iter()
            .map(|m| AgentMessage {
                content: m.content.clone(),
                role: match m.role {
                    AgentRole::Assistant => AgentRole::User,
                    AgentRole::System => AgentRole::System,
                    AgentRole::User => AgentRole::Assistant,
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
        let completion = self
            .state
            .agents
            .prompt_with(PromptRequest {
                messages,
                model: &persona.model,
                preamble: &persona.preamble,
                provider,
            })
            .await
            .map_err(RunError::Persona)?;
        Ok(completion.text)
    }

    async fn resolve_target(&self, target: &str, user_id: UserId) -> ResolvedTarget {
        let bandit_scores = match self.state.experiments.bandit_query(target) {
            None => Vec::new(),
            Some(query) => self
                .state
                .judge_store
                .mean_scores_by_agent(query)
                .await
                .unwrap_or_default(),
        };
        let resolved = self
            .state
            .experiments
            .resolve_with_scores(target, user_id, &bandit_scores);
        ResolvedTarget {
            agent: resolved.agent.into_owned(),
            experiment: resolved.experiment.map(std::borrow::ToOwned::to_owned),
        }
    }

    /// One synthetic conversation. Errors bubble up so the spawn wrapper
    /// can mark the run failed; success paths mark it completed inline.
    async fn run_once(&self, config: SmokeTestConfig, run_id: RunId) -> Result<(), RunError> {
        let synthetic_user = UserId::new();
        let mut messages: Vec<AgentMessage> = Vec::new();
        let mut resolved_recorded = false;

        for turn_index in 0..config.max_turns {
            let persona_text = if turn_index == 0
                && let Some(initial) = config.initial_message.as_ref()
            {
                initial.clone()
            } else {
                self.persona_turn(&config.persona, &messages).await?
            };
            self.store
                .record_persona_turn(run_id, turn_index, &persona_text)
                .await?;
            messages.push(AgentMessage {
                content: persona_text.clone(),
                role: AgentRole::User,
            });
            if matches_marker(&persona_text, config.stop_marker.as_deref()) {
                break;
            }

            let assistant_message_id = MessageId::new();
            let resolved = self.resolve_target(&config.target, synthetic_user).await;
            if !resolved_recorded {
                self.store
                    .set_resolution(run_id, &resolved.agent, resolved.experiment.as_deref())
                    .await?;
                resolved_recorded = true;
            }
            let span = info_span!(
                "smoke_turn",
                agent = %resolved.agent,
                run_id = %run_id.0,
                turn_id = %assistant_message_id.0,
                user_id = %synthetic_user.0,
            );
            let completion = self
                .state
                .agents
                .complete(&resolved.agent, messages.clone(), synthetic_user)
                .instrument(span)
                .await?;
            self.store
                .record_assistant_turn(run_id, turn_index, assistant_message_id, &completion.text)
                .await?;
            messages.push(AgentMessage {
                content: completion.text.clone(),
                role: AgentRole::Assistant,
            });

            let judges = self.state.judges_for_agent(&resolved.agent);
            self.state.judge_store.spawn_score(
                Arc::clone(&self.state.agents),
                judges::ScoredExchange {
                    agent_name: resolved.agent.clone(),
                    assistant_message: completion.text.clone(),
                    message_id: assistant_message_id,
                    user_id: synthetic_user,
                    user_message: persona_text.clone(),
                },
                judges,
            );

            if matches_marker(&completion.text, config.stop_marker.as_deref()) {
                break;
            }
        }

        self.store
            .finish_run(run_id, RunStatus::Completed, None)
            .await?;
        Ok(())
    }

    /// A handle a spawned run can own.
    fn share(&self) -> Self {
        Self {
            configs: self.configs.clone(),
            state: Arc::clone(&self.state),
            store: Arc::clone(&self.store),
        }
    }
}

struct ResolvedTarget {
    agent: String,
    experiment: Option<String>,
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
    Agent(#[from] AgentsError),
    #[error("persona: {0}")]
    Persona(#[source] AgentsError),
    #[error("store: {0}")]
    Store(#[from] SmokeStoreError),
    #[error("persona: unknown persona provider '{0}'")]
    UnknownPersonaProvider(String),
}
