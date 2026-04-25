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

use config::ExperimentConfig;
use judge::{Judge, spawn_score};
use memory::{MessageId, Store, UserId};
use prompter::{Message as PrompterMessage, Prompter};
use telemetry::{Ctx as TelemetryCtx, TurnId};

use crate::server::AppState;
use crate::server::judges_for_agent;

/// Spawn a background task per non-primary variant. Each task runs the
/// variant against the same prepared context the primary saw, scores
/// its output, and persists the scores. Failures are logged and
/// swallowed — shadow is best-effort.
#[allow(clippy::too_many_arguments)]
pub fn spawn_shadow_runs<P: Prompter + 'static>(
    state: Arc<AppState<P>>,
    experiment: &ExperimentConfig,
    parent_turn: TurnId,
    user_id: UserId,
    user_message: String,
    messages: Vec<PrompterMessage>,
) {
    if !state
        .prompter
        .router()
        .shadow_should_sample(experiment, user_id)
    {
        return;
    }
    let variants: Vec<String> = state
        .prompter
        .router()
        .shadow_variants(experiment)
        .map(|v| v.agent.clone())
        .collect();
    for agent_name in variants {
        let state = Arc::clone(&state);
        let messages = messages.clone();
        let user_message = user_message.clone();
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

async fn run_shadow<P: Prompter + 'static>(
    state: Arc<AppState<P>>,
    parent_turn: TurnId,
    user_id: UserId,
    agent_name: String,
    user_message: String,
    messages: Vec<PrompterMessage>,
) {
    let shadow_message_id = MessageId::new();
    // Reuse the parent turn's correlation id so shadow events nest
    // under the same turn tree in the studio. No `parent` event id —
    // shadow runs are sibling roots within the turn.
    let ctx = TelemetryCtx {
        correlation_id: parent_turn,
        parent: None,
        user_id,
    };
    let outcome = state.prompter.complete(&agent_name, messages, ctx).await;
    match outcome {
        Ok(completion) => {
            let judges: Vec<Arc<Judge>> = judges_for_agent(&state, &agent_name);
            spawn_score(
                judges,
                Arc::<Store>::clone(&state.memory),
                Arc::clone(&state.prompter),
                user_id,
                shadow_message_id,
                agent_name,
                user_message,
                completion.text,
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
