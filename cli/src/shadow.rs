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
use judges::Judge;
use memory::{MessageId, UserId};
use telemetry::TurnId;
use tracing::{Instrument, info_span};

use crate::server::AppState;

/// One variant's replay of a served request: the same context the primary
/// consumed, attributed to the parent turn.
pub struct ShadowRun {
    pub agent_name: String,
    pub messages: Vec<AgentMessage>,
    pub parent_turn: TurnId,
    pub user_id: UserId,
    pub user_message: String,
}

impl ShadowRun {
    /// Run in the background. Failures are logged and swallowed — shadow
    /// is best-effort.
    pub fn spawn<P: Agents + OneShotPrompt + 'static>(self, state: Arc<AppState<P>>) {
        tokio::spawn(async move {
            self.run(state).await;
        });
    }

    async fn run<P: Agents + OneShotPrompt + 'static>(self, state: Arc<AppState<P>>) {
        let shadow_message_id = MessageId::new();
        // WHY: reuse the parent turn's correlation id so shadow events nest
        // under the same turn tree in the studio — a fresh `turn` span with
        // the same `turn_id` keeps every nested `tool_call` span linked to
        // the original request in the events table.
        let span = info_span!(
            "turn",
            agent = %self.agent_name,
            turn_id = %self.parent_turn.0,
            user_id = %self.user_id.0,
            user_message = %self.user_message,
        );
        let outcome = state
            .agents
            .complete(&self.agent_name, self.messages, self.user_id)
            .instrument(span)
            .await;
        match outcome {
            Err(err) => {
                tracing::warn!(
                    user = %self.user_id.0,
                    agent = %self.agent_name,
                    error = %err,
                    "shadow run failed",
                );
            }
            Ok(completion) => {
                let judges: Vec<Arc<Judge>> = state.judges_for_agent(&self.agent_name);
                state.judge_store.spawn_score(
                    Arc::clone(&state.agents),
                    judges::ScoredExchange {
                        agent_name: self.agent_name,
                        assistant_message: completion.text,
                        message_id: shadow_message_id,
                        user_id: self.user_id,
                        user_message: self.user_message,
                    },
                    judges,
                );
            }
        }
    }
}
