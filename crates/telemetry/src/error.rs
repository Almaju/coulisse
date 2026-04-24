use thiserror::Error;

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("telemetry database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("telemetry payload serialize error: {0}")]
    Payload(#[from] serde_json::Error),
}
