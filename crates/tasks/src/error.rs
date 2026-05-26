use coulisse_core::migrate::MigrateError;

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("malformed task row {id}: unknown {field} {value:?}")]
    MalformedRow {
        field: &'static str,
        id: String,
        value: String,
    },
    #[error("schema migration error: {0}")]
    Migrate(#[from] MigrateError),
}
