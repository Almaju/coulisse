use thiserror::Error;

#[derive(Debug, Error)]
pub enum LimitError {
    #[error("{window} token limit exceeded: used {used}/{limit}, retry after {retry_after}s")]
    Exceeded {
        limit: u64,
        retry_after: u64,
        used: u64,
        window: WindowKind,
    },
    #[error("metadata key '{key}' must be a non-negative integer, got '{value}'")]
    InvalidMetadata { key: String, value: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowKind {
    Day,
    Hour,
    Month,
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
