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
use crate::config::Config;
use crate::paths::StatePaths;

pub struct Options {
    pub yes: bool,
}

/// # Errors
///
/// Returns an error if a server is still running, the config can't be loaded,
/// or a database file can't be removed.
pub fn run(config_path: &Path, opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    // Deleting the file out from under a running server (WAL mode keeps the
    // fd open) leaves it writing to an unlinked inode — refuse instead.
    let paths = StatePaths::for_config(config_path);
    if let Some(pid) = read_pid(&paths.pid)
        && pid_alive(pid)
    {
        return Err(format!(
            "coulisse is running (pid {pid}) — stop it first with `coulisse stop`, then reset"
        )
        .into());
    }

    let config = Config::from_path(config_path)?;
    let state_dir = crate::secrets::state_dir_for(config_path);
    let memory_config =
        crate::memory_resolve::resolve_memory(&config.memory, &config.providers, &state_dir)?;

    let db_path = match memory_config.backend {
        BackendConfig::InMemory => {
            println!("memory backend is in-memory (ephemeral) — nothing on disk to reset");
            return Ok(());
        }
        BackendConfig::Sqlite { path } => path,
    };

    // SQLite in WAL mode leaves `-wal` / `-shm` sidecars next to the main
    // file; remove all three so no stale pages survive the reset.
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

    if !opts.yes && !confirm()? {
        println!("aborted — nothing deleted");
        return Ok(());
    }

    for p in &existing {
        std::fs::remove_file(p).map_err(|e| format!("failed to remove {}: {e}", p.display()))?;
    }
    println!("removed {} file(s) — database reset", existing.len());
    Ok(())
}

/// The main database file plus its WAL/SHM sidecars.
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
