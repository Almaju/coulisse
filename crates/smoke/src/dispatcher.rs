use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

use crate::types::RunId;

/// Hands off a freshly-allocated smoke run to whoever owns the agent
/// runtime + judge wiring. Implemented in `cli` (which can see `agents`
/// and `judges`); consumed by the smoke admin router so the
/// "Run now" button does not require this crate to depend on `agents`
/// or `judges` directly.
pub trait RunDispatcher: Send + Sync {
    fn dispatch<'a>(
        &'a self,
        test_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RunId>, DispatchError>> + Send + 'a>>;
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("smoke test '{0}' not found")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

impl DispatchError {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}
