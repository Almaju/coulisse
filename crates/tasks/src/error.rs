use coulisse_core::UnknownTaskState;
use coulisse_core::migrate::MigrateError;

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("malformed task row {id}: id is not a UUID")]
    InvalidId {
        id: String,
        #[source]
        source: uuid::Error,
    },
    #[error("malformed task row {id}: user id {user_id:?} is not a UUID")]
    InvalidUserId {
        id: String,
        #[source]
        source: uuid::Error,
        user_id: String,
    },
    #[error("schema migration error: {0}")]
    Migrate(#[from] MigrateError),
    #[error("malformed task row {id}: {source}")]
    UnknownState {
        id: String,
        #[source]
        source: UnknownTaskState,
    },
}
