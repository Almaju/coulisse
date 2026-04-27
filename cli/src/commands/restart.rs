//! `coulisse restart` — stop (if running) then start.

use std::path::Path;

use crate::commands::{start, stop};

/// # Errors
///
/// Returns an error if the underlying operation fails.
pub fn run(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    stop::run(config_path, &stop::Options { force: false })?;
    start::run(
        config_path,
        &start::Options {
            detached_child: false,
            foreground: false,
        },
    )?;
    Ok(())
}
