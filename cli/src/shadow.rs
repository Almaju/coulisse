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

use agents::{Agents, Message as AgentMessage};
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
    if !state.experiments.shadow_should_sample(experiment, user_id) {
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
            run_shadow(
                state,
                parent_turn,
                user_id,
                agent_name,
                user_message,
                messages,
            )
            .await;
        });
    }
}

async fn run_shadow<P: Agents + OneShotPrompt + 'static>(
    state: Arc<AppState<P>>,
    parent_turn: TurnId,
    user_id: UserId,
    agent_name: String,
    user_message: String,
    messages: Vec<AgentMessage>,
) {
    let shadow_message_id = MessageId::new();
    // Reuse the parent turn's correlation id so shadow events nest
    // under the same turn tree in the studio: a fresh `turn` span with
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
        .complete(&agent_name, messages, user_id)
        .instrument(span)
        .await;
    match outcome {
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
        }
        Err(err) => {
            tracing::warn!(
                user = %user_id.0,
                agent = %agent_name,
                error = %err,
                "shadow run failed",
            );
        }
    }
}
