use std::str::FromStr;
use std::sync::Arc;

use ::cron::Schedule;
use chrono::{DateTime, Utc};
use coulisse_core::{TaskQueue, TaskSubmission, UserId, now_secs};
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
        schedule
            .parse::<CronSchedule>()
            .map_err(|source| TriggerError::InvalidCronSchedule {
                name: t.name.clone(),
                schedule: schedule.clone(),
                source,
            })?;
    }
    Ok(())
}

/// A parsed cron expression. Accepts 5-field POSIX cron by normalising to
/// 6-field with leading seconds: the `cron` crate requires the seconds
/// field; most humans don't remember that.
#[derive(Clone, Debug)]
pub(crate) struct CronSchedule(Schedule);

impl CronSchedule {
    /// The first fire strictly after `now`.
    fn next_fire_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.0.after(&now).next()
    }
}

impl FromStr for CronSchedule {
    type Err = ::cron::error::Error;

    fn from_str(expr: &str) -> Result<Self, Self::Err> {
        let trimmed = expr.trim();
        let field_count = trimmed.split_whitespace().count();
        let normalized = if field_count == 5 {
            format!("0 {trimmed}")
        } else {
            trimmed.to_string()
        };
        Schedule::from_str(&normalized).map(Self)
    }
}

pub(crate) struct CronTrigger {
    agent: String,
    name: String,
    prompt: String,
    schedule: CronSchedule,
    user_id: UserId,
}

impl CronTrigger {
    /// `None` when `config` is not a cron trigger or its schedule does not
    /// parse (`validate_all` is where a bad schedule is surfaced).
    pub(crate) fn from_config(config: &TriggerConfig, user_id: UserId) -> Option<Self> {
        let TriggerKind::Cron { schedule } = &config.kind else {
            return None;
        };
        let schedule = schedule.parse::<CronSchedule>().ok()?;
        Some(Self {
            agent: config.agent.clone(),
            name: config.name.clone(),
            prompt: config.prompt.clone(),
            schedule,
            user_id,
        })
    }

    pub(crate) fn spawn(self, queue: Arc<dyn TaskQueue>) {
        tokio::spawn(async move {
            self.run(queue).await;
        });
    }

    async fn run(self, queue: Arc<dyn TaskQueue>) {
        info!(trigger = %self.name, agent = %self.agent, "cron trigger armed");
        loop {
            let now = unix_now();
            let Some(next) = self.schedule.next_fire_after(now) else {
                error!(trigger = %self.name, "cron schedule yielded no future fire — exiting");
                return;
            };
            let Ok(delta) = next.signed_duration_since(now).to_std() else {
                // Next fire is already past; loop without sleeping so we don't
                // spin if the schedule somehow yields stale times.
                continue;
            };
            tokio::time::sleep(delta).await;
            let submission = TaskSubmission {
                agent: &self.agent,
                prompt: &self.prompt,
                user_id: self.user_id,
            };
            match queue.submit(submission).await {
                Ok(task_id) => {
                    info!(
                        trigger = %self.name,
                        agent = %self.agent,
                        task_id = %task_id.0,
                        "cron trigger fired",
                    );
                }
                Err(e) => {
                    error!(trigger = %self.name, %e, "cron trigger failed to enqueue");
                }
            }
        }
    }
}

/// The current wall-clock second as a UTC instant, read through the
/// workspace's single clock seam.
fn unix_now() -> DateTime<Utc> {
    let secs = i64::try_from(now_secs()).unwrap_or(i64::MAX);
    DateTime::<Utc>::from_timestamp(secs, 0).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::CronSchedule;
    use chrono::{DateTime, Utc};

    #[test]
    fn five_field_normalises() {
        assert!("0 9 * * *".parse::<CronSchedule>().is_ok());
    }

    #[test]
    fn six_field_passes_through() {
        assert!("0 0 9 * * *".parse::<CronSchedule>().is_ok());
    }

    #[test]
    fn garbage_rejected() {
        assert!("not a cron".parse::<CronSchedule>().is_err());
    }

    #[test]
    fn every_minute_works() {
        assert!("* * * * *".parse::<CronSchedule>().is_ok());
    }

    #[test]
    fn next_fire_is_strictly_after_now() {
        let schedule: CronSchedule = "0 9 * * *".parse().unwrap();
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let next = schedule.next_fire_after(now).unwrap();
        assert!(next > now);
        assert_eq!(next.format("%H:%M:%S").to_string(), "09:00:00");
        let at_fire = schedule.next_fire_after(next).unwrap();
        assert_eq!(at_fire - next, chrono::Duration::days(1));
    }
}
