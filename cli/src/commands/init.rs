//! `coulisse init` — write a starter `coulisse.yaml`.

use std::fs;
use std::io;
use std::path::Path;

const MINIMAL_TEMPLATE: &str = include_str!("init_template.yaml");
const FULL_EXAMPLE: &str = include_str!("../../../coulisse.example.yaml");

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("config already exists at {0} — pass --force to overwrite")]
    AlreadyExists(String),
    #[error("failed to write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
}

pub struct Options {
    pub force: bool,
    pub from_example: bool,
}

/// # Errors
///
/// Returns an error if the underlying operation fails.
pub fn run(config_path: &Path, opts: &Options) -> Result<(), InitError> {
    if config_path.exists() && !opts.force {
        return Err(InitError::AlreadyExists(config_path.display().to_string()));
    }
    let body = if opts.from_example {
        FULL_EXAMPLE
    } else {
        MINIMAL_TEMPLATE
    };
    fs::write(config_path, body).map_err(|source| InitError::Write {
        path: config_path.display().to_string(),
        source,
    })?;
    println!("wrote {}", config_path.display());
    if !opts.from_example {
        println!(
            "edit it to set your provider API key, then run `coulisse start` to launch the server."
        );
    }
    Ok(())
}
