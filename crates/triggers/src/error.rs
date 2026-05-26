#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    #[error("trigger '{name}' has invalid cron schedule '{schedule}': {reason}")]
    InvalidCronSchedule {
        name: String,
        reason: String,
        schedule: String,
    },
}
