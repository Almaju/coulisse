use coulisse_core::BoxFuture;
use thiserror::Error;

use crate::store::SmokeStoreError;
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
    ) -> BoxFuture<'a, Result<Vec<RunId>, DispatchError>>;
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("smoke test '{0}' not found")]
    NotFound(String),
    #[error("failed to allocate smoke run: {0}")]
    Store(#[from] SmokeStoreError),
}
