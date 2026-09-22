//! `coulisse restart` — stop (if running) then start.

use std::path::Path;

use crate::commands::{start, stop};

#[derive(Debug, thiserror::Error)]
pub enum RestartError {
    #[error(transparent)]
    Start(#[from] start::StartError),
    #[error(transparent)]
    Stop(#[from] stop::StopError),
}

/// # Errors
///
/// Returns an error if the running server cannot be stopped or the new
/// one fails to come up.
pub fn run(config_path: &Path) -> Result<(), RestartError> {
    stop::Options { force: false }.run(config_path)?;
    start::Options {
        detached_child: false,
        foreground: false,
    }
    .run(config_path)?;
    Ok(())
}
