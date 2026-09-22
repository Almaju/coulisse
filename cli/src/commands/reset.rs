//! `coulisse reset` — delete the `SQLite` database, wiping every bit of stored
//! state (conversation memory, long-term memories, telemetry, judge scores,
//! rate-limit windows, background tasks, API tokens). The `coulisse.yaml` is
//! never touched. Destructive and irreversible, so it refuses to run while a
//! server holds the database open and confirms interactively unless `-y` is
//! passed.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use memory::BackendConfig;

use crate::commands::status::{pid_alive, read_pid};
use crate::config::{Config, ConfigError};
use crate::memory_resolve::{MemoryResolveError, MemoryResolver};
use crate::paths::StatePaths;

#[derive(Debug, thiserror::Error)]
pub enum ResetError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("failed to read the confirmation: {0}")]
    Confirm(#[source] io::Error),
    #[error(transparent)]
    MemoryResolve(#[from] MemoryResolveError),
    #[error("failed to remove {path}: {source}")]
    Remove {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("coulisse is running (pid {0}) — stop it first with `coulisse stop`, then reset")]
    Running(i32),
}

pub struct Options {
    pub yes: bool,
}

impl Options {
    /// # Errors
    ///
    /// Returns an error if a server is still running, the config can't be
    /// loaded, or a database file can't be removed.
    pub fn run(&self, config_path: &Path) -> Result<(), ResetError> {
        // Deleting the file out from under a running server (WAL mode keeps the
        // fd open) leaves it writing to an unlinked inode — refuse instead.
        let paths = StatePaths::for_config(config_path);
        if let Some(pid) = read_pid(&paths.pid)
            && pid_alive(pid)
        {
            return Err(ResetError::Running(pid));
        }

        let config = Config::from_path(config_path)?;
        let state_dir = crate::secrets::state_dir_for(config_path);
        let memory_config = MemoryResolver {
            providers: &config.providers,
            state_dir: &state_dir,
        }
        .resolve(&config.memory)?;

        let db_path = match memory_config.backend {
            BackendConfig::InMemory => {
                println!("memory backend is in-memory (ephemeral) — nothing on disk to reset");
                return Ok(());
            }
            BackendConfig::Sqlite { path } => path,
        };

        let existing: Vec<PathBuf> = sqlite_files(&db_path)
            .into_iter()
            .filter(|p| p.exists())
            .collect();
        if existing.is_empty() {
            println!(
                "no database found at {} — nothing to reset",
                db_path.display()
            );
            return Ok(());
        }

        eprintln!("⚠️  This permanently deletes the Coulisse database:");
        for p in &existing {
            eprintln!("      {}", p.display());
        }
        eprintln!(
            "    Wipes conversation memory, long-term memories, telemetry, judge\n    \
             scores, rate-limit windows, background tasks, and API tokens.\n    \
             Your coulisse.yaml is NOT touched. This cannot be undone."
        );

        if !self.yes && !confirm().map_err(ResetError::Confirm)? {
            println!("aborted — nothing deleted");
            return Ok(());
        }

        for p in &existing {
            std::fs::remove_file(p).map_err(|source| ResetError::Remove {
                path: p.display().to_string(),
                source,
            })?;
        }
        println!("removed {} file(s) — database reset", existing.len());
        Ok(())
    }
}

/// The main database file plus its WAL/SHM sidecars. `SQLite` in WAL mode
/// leaves `-wal` / `-shm` next to the main file; all three go so no stale
/// pages survive the reset.
fn sqlite_files(db: &Path) -> Vec<PathBuf> {
    let mut out = vec![db.to_path_buf()];
    for suffix in ["-shm", "-wal"] {
        let mut name = db.as_os_str().to_os_string();
        name.push(suffix);
        out.push(PathBuf::from(name));
    }
    out
}

fn confirm() -> io::Result<bool> {
    print!("Type 'y' to confirm, anything else to abort: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}
