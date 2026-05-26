use std::str::FromStr;
use std::sync::Arc;

use ::cron::Schedule;
use chrono::Utc;
use coulisse_core::{TaskQueue, UserId};
use tracing::{error, info};

use crate::config::{TriggerConfig, TriggerKind};
use crate::error::TriggerError;

/// Validate every cron trigger's schedule. Call once at startup; if it
/// returns Err, refuse to boot rather than letting a bad expression silently
/// disable one trigger forever. Non-cron variants are ignored here — they
/// have their own validators.
///
/// # Errors
///
/// Returns an error if any cron schedule fails to parse.
pub fn validate_all(triggers: &[TriggerConfig]) -> Result<(), TriggerError> {
    for t in triggers {
        let TriggerKind::Cron { schedule } = &t.kind else {
            continue;
        };
        parse_schedule(schedule).map_err(|reason| TriggerError::InvalidCronSchedule {
            name: t.name.clone(),
            reason,
            schedule: schedule.clone(),
        })?;
    }
    Ok(())
}

/// Spawn one tokio task per cron trigger. Each task sleeps until the next
/// scheduled fire, enqueues a task via the `TaskQueue` trait, repeats.
///
/// Webhook triggers are ignored; they're served by `webhook_router`
/// instead. Callers should have called `validate_all` first; this function
/// silently skips triggers with unparseable schedules to keep the runtime
/// crash-free, but the boot-time validator is the right place to surface
/// those errors.
pub fn spawn_cron(triggers: &[TriggerConfig], queue: Arc<dyn TaskQueue>, user_id: UserId) {
    for trigger in triggers {
        let TriggerKind::Cron { schedule } = &trigger.kind else {
            continue;
        };
        let Ok(schedule) = parse_schedule(schedule) else {
            continue;
        };
        let queue = Arc::clone(&queue);
        let name = trigger.name.clone();
        let agent = trigger.agent.clone();
        let prompt = trigger.prompt.clone();
        tokio::spawn(async move {
            run_loop(name, agent, prompt, schedule, queue, user_id).await;
        });
    }
}

/// Accept 5-field POSIX cron by normalising to 6-field with leading
/// seconds. The `cron` crate requires the seconds field; most humans don't
/// remember that.
fn parse_schedule(expr: &str) -> Result<Schedule, String> {
    let trimmed = expr.trim();
    let field_count = trimmed.split_whitespace().count();
    let normalized = if field_count == 5 {
        format!("0 {trimmed}")
    } else {
        trimmed.to_string()
    };
    Schedule::from_str(&normalized).map_err(|e| e.to_string())
}

async fn run_loop(
    name: String,
    agent: String,
    prompt: String,
    schedule: Schedule,
    queue: Arc<dyn TaskQueue>,
    user_id: UserId,
) {
    info!(trigger = %name, agent = %agent, "cron trigger armed");
    loop {
        let now = Utc::now();
        let Some(next) = schedule.upcoming(Utc).next() else {
            error!(trigger = %name, "cron schedule yielded no future fire — exiting");
            return;
        };
        let Ok(delta) = next.signed_duration_since(now).to_std() else {
            // Next fire is already past; loop without sleeping so we don't
            // spin if the schedule somehow yields stale times.
            continue;
        };
        tokio::time::sleep(delta).await;
        match queue.submit(&agent, &prompt, user_id).await {
            Ok(task_id) => {
                info!(trigger = %name, agent = %agent, task_id = %task_id.0, "cron trigger fired");
            }
            Err(e) => {
                error!(trigger = %name, %e, "cron trigger failed to enqueue");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_schedule;

    #[test]
    fn five_field_normalises() {
        assert!(parse_schedule("0 9 * * *").is_ok());
    }

    #[test]
    fn six_field_passes_through() {
        assert!(parse_schedule("0 0 9 * * *").is_ok());
    }

    #[test]
    fn garbage_rejected() {
        assert!(parse_schedule("not a cron").is_err());
    }

    #[test]
    fn every_minute_works() {
        assert!(parse_schedule("* * * * *").is_ok());
    }
}
