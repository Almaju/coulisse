//! Shadow-strategy plumbing. Runs the non-primary variants of a
//! `shadow` experiment in the background after the primary has been
//! served, scores their outputs through the variant's own judges, and
//! attributes the resulting scores to the variant agent name so
//! per-variant aggregation works downstream (studio + bandit).
//!
//! Shadow runs never write to the user's message history — only to
//! scores. The user's conversation continues to reflect only what the
//! primary actually returned.

use std::sync::Arc;

use agents::{Agents, CompletionRequest, Message as AgentMessage};
use coulisse_core::OneShotPrompt;
use experiments::ExperimentConfig;
use judges::{Judge, spawn_score};
use memory::{MessageId, UserId};
use telemetry::TurnId;
use tracing::{Instrument, info_span};

use crate::server::AppState;
use crate::server::judges_for_agent;

/// Spawn a background task per non-primary variant. Each task runs the
/// variant against the same prepared context the primary saw, scores
/// its output, and persists the scores. Failures are logged and
/// swallowed — shadow is best-effort.
#[allow(clippy::too_many_arguments)]
pub fn spawn_shadow_runs<P: Agents + OneShotPrompt + 'static>(
    state: &Arc<AppState<P>>,
    experiment: &ExperimentConfig,
    parent_turn: TurnId,
    user_id: UserId,
    user_message: &str,
    messages: &[AgentMessage],
) {
    if !experiment.shadow_should_sample(user_id) {
        return;
    }
    let variants: Vec<String> = state
        .experiments
        .shadow_variants(experiment)
        .map(|v| v.agent.clone())
        .collect();
    for agent_name in variants {
        let state = Arc::clone(state);
        let messages = messages.to_vec();
        let user_message = user_message.to_string();
        tokio::spawn(async move {
            run_shadow(ShadowRun {
                agent_name,
                messages,
                parent_turn,
                state,
                user_id,
                user_message,
            })
            .await;
        });
    }
}

struct ShadowRun<P: Agents + OneShotPrompt> {
    agent_name: String,
    messages: Vec<AgentMessage>,
    parent_turn: TurnId,
    state: Arc<AppState<P>>,
    user_id: UserId,
    user_message: String,
}

async fn run_shadow<P: Agents + OneShotPrompt + 'static>(inputs: ShadowRun<P>) {
    let ShadowRun {
        agent_name,
        messages,
        parent_turn,
        state,
        user_id,
        user_message,
    } = inputs;
    let shadow_message_id = MessageId::new();
    // WHY: reuse the parent turn's correlation id so shadow events nest
    // under the same turn tree in the studio — a fresh `turn` span with
    // the same `turn_id` keeps every nested `tool_call` span linked to
    // the original request in the events table.
    let span = info_span!(
        "turn",
        agent = %agent_name,
        turn_id = %parent_turn.0,
        user_id = %user_id.0,
        user_message = %user_message,
    );
    let outcome = state
        .agents
        .complete(CompletionRequest {
            agent_name: &agent_name,
            messages,
            user_id,
        })
        .instrument(span)
        .await;
    match outcome {
        Err(err) => {
            tracing::warn!(
                user = %user_id.0,
                agent = %agent_name,
                error = %err,
                "shadow run failed",
            );
        },
        Ok(completion) => {
            let judges: Vec<Arc<Judge>> = judges_for_agent(&state, &agent_name);
            spawn_score(
                judges,
                Arc::clone(&state.judge_store),
                Arc::clone(&state.agents),
                judges::ScoredExchange {
                    agent_name,
                    assistant_message: completion.text,
                    message_id: shadow_message_id,
                    user_id,
                    user_message,
                },
            );
        },
    }
}
