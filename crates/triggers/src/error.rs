#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    #[error("trigger '{name}' has invalid cron schedule '{schedule}': {source}")]
    InvalidCronSchedule {
        name: String,
        schedule: String,
        #[source]
        source: ::cron::error::Error,
    },
}
