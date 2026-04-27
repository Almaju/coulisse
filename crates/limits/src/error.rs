use thiserror::Error;

#[derive(Debug, Error)]
pub enum LimitError {
    #[error("rate limit database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("{window} token limit exceeded: used {used}/{limit}, retry after {retry_after}s")]
    Exceeded {
        limit: u64,
        retry_after: u64,
        used: u64,
        window: WindowKind,
    },
    #[error("metadata key '{key}' must be a non-negative integer, got '{value}'")]
    InvalidMetadata { key: String, value: String },
    #[error("schema migration failed: {0}")]
    Migrate(#[from] coulisse_core::migrate::MigrateError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowKind {
    Day,
    Hour,
    Month,
}

impl WindowKind {
    /// Stable identifier used as the `kind` column value in `SQLite`. Distinct
    /// from `Display`, which produces the user-facing word ("daily"); this
    /// produces the noun ("day") so renaming the displayed form doesn't
    /// silently migrate stored rows into a new bucket.
    pub(crate) const fn as_db_str(self) -> &'static str {
        match self {
            WindowKind::Day => "day",
            WindowKind::Hour => "hour",
            WindowKind::Month => "month",
        }
    }

    pub(crate) const fn size_secs(self) -> u64 {
        const SECS_PER_DAY: u64 = 86_400;
        const SECS_PER_HOUR: u64 = 3_600;
        const SECS_PER_MONTH: u64 = 30 * SECS_PER_DAY;
        match self {
            WindowKind::Day => SECS_PER_DAY,
            WindowKind::Hour => SECS_PER_HOUR,
            WindowKind::Month => SECS_PER_MONTH,
        }
    }
}

impl std::fmt::Display for WindowKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Day => "daily",
            Self::Hour => "hourly",
            Self::Month => "monthly",
        })
    }
}
