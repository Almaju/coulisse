//! `coulisse stop` — terminate a detached server via its PID file.

use std::fs;
use std::path::Path;
use std::time::Duration;

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

use crate::commands::status::{pid_alive, read_pid};
use crate::paths::StatePaths;

const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the exit wait re-checks the process. The timeout is
/// expressed as a number of these polls so the wait never reads the
/// clock.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

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

impl Options {
    /// # Errors
    ///
    /// Returns an error if the signal cannot be delivered or the server
    /// is still alive once the stop timeout has elapsed.
    pub fn run(&self, config_path: &Path) -> Result<(), StopError> {
        let paths = StatePaths::for_config(config_path);
        let Some(pid) = read_pid(&paths.pid) else {
            println!("not running");
            return Ok(());
        };
        if !pid_alive(pid) {
            let _ = fs::remove_file(&paths.pid);
            println!("not running (cleaned up stale pid file)");
            return Ok(());
        }

        let signal = if self.force {
            Signal::SIGKILL
        } else {
            Signal::SIGTERM
        };
        signal::kill(Pid::from_raw(pid), signal).map_err(|source| StopError::Kill {
            pid,
            signal,
            source,
        })?;

        if !wait_for_exit(pid) {
            return Err(StopError::Timeout {
                pid,
                timeout: STOP_TIMEOUT,
            });
        }
        let _ = fs::remove_file(&paths.pid);
        println!("stopped (pid {pid})");
        Ok(())
    }
}

/// Poll until `pid` is gone; false once `STOP_TIMEOUT` worth of polls
/// have passed with it still alive.
fn wait_for_exit(pid: i32) -> bool {
    let polls = STOP_TIMEOUT.as_millis() / POLL_INTERVAL.as_millis();
    for _ in 0..polls {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    !pid_alive(pid)
}
