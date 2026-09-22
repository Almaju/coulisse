use thiserror::Error;

use crate::event::UnknownEventKind;

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("telemetry database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("telemetry column {column} holds {value:?}, which is not a UUID")]
    InvalidUuid {
        column: &'static str,
        #[source]
        source: uuid::Error,
        value: String,
    },
    #[error("schema migration failed: {0}")]
    Migrate(#[from] coulisse_core::migrate::MigrateError),
    #[error("telemetry payload serialize error: {0}")]
    Payload(#[from] serde_json::Error),
    #[error("telemetry row: {0}")]
    UnknownEventKind(#[from] UnknownEventKind),
    #[error("telemetry row: {0}")]
    UnknownToolCallKind(#[from] coulisse_core::UnknownToolCallKind),
}
