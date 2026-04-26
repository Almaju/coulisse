//! `coulisse stop` — terminate a detached server via its PID file.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

use crate::commands::status::{pid_alive, read_pid};
use crate::paths::StatePaths;

const STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum StopError {
    #[error("kill({pid}, {signal:?}) failed: {source}")]
    Kill {
        pid: i32,
        signal: Signal,
        #[source]
        source: nix::Error,
    },
    #[error("server (pid {pid}) didn't exit within {0:?} — try `coulisse stop --force`", .timeout)]
    Timeout { pid: i32, timeout: Duration },
}

pub struct Options {
    pub force: bool,
}

pub fn run(config_path: &Path, opts: Options) -> Result<(), StopError> {
    let paths = StatePaths::for_config(config_path);
    let pid = match read_pid(&paths.pid) {
        Some(pid) => pid,
        None => {
            println!("not running");
            return Ok(());
        }
    };
    if !pid_alive(pid) {
        let _ = fs::remove_file(&paths.pid);
        println!("not running (cleaned up stale pid file)");
        return Ok(());
    }

    let signal = if opts.force {
        Signal::SIGKILL
    } else {
        Signal::SIGTERM
    };
    signal::kill(Pid::from_raw(pid), signal).map_err(|source| StopError::Kill {
        pid,
        signal,
        source,
    })?;

    let deadline = Instant::now() + STOP_TIMEOUT;
    while pid_alive(pid) {
        if Instant::now() >= deadline {
            return Err(StopError::Timeout {
                pid,
                timeout: STOP_TIMEOUT,
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_file(&paths.pid);
    println!("stopped (pid {pid})");
    Ok(())
}
