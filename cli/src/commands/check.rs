//! `coulisse check` — load and validate the YAML config without
//! starting the server. Catches schema and cross-reference errors
//! (agent → provider, agent → judge, experiment variant → agent, ...)
//! before a real `start`.

use std::path::Path;

use crate::config::Config;

/// # Errors
///
/// Returns an error if the underlying operation fails.
pub fn run(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_path(config_path)?;
    println!(
        "ok — {} ({} agents, {} judges, {} experiments, {} providers)",
        config_path.display(),
        config.agents.len(),
        config.judges.len(),
        config.experiments.len(),
        config.providers.len(),
    );
    Ok(())
}
