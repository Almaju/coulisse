//! `coulisse status` — report whether the server is running.

use std::fs;
use std::path::Path;

use nix::errno::Errno;
use nix::sys::signal;
use nix::unistd::Pid;

use crate::paths::StatePaths;

pub fn run(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let paths = StatePaths::for_config(config_path);
    match read_pid(&paths.pid) {
        Some(pid) if pid_alive(pid) => {
            println!("running (pid {pid})");
            println!("  config: {}", paths.config.display());
            println!("  log:    {}", paths.log.display());
        }
        Some(pid) => {
            println!(
                "not running (stale pid file at {} held pid {pid})",
                paths.pid.display()
            );
        }
        None => {
            println!("not running");
        }
    }
    Ok(())
}

pub fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `kill(pid, 0)` returns success if the process exists and we can
/// signal it, ESRCH if it doesn't exist, EPERM if it exists but we
/// can't signal it (still alive).
pub fn pid_alive(pid: i32) -> bool {
    match signal::kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}
