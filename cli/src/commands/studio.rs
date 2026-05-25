//! `coulisse studio` (alias `admin`) — open the studio UI in a browser.
//!
//! Reads the port out of the config and hands the URL to the OS's
//! "open this in the default browser" handler. The detached server must
//! already be running; we don't start it here so the side effect of
//! running this command stays predictable (one foreground process: the
//! browser).

use std::io;
use std::path::Path;
use std::process::Command;

use crate::commands::status::{pid_alive, read_pid};
use crate::config::Config;
use crate::paths::StatePaths;

#[derive(Debug, thiserror::Error)]
pub enum StudioError {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error("failed to launch browser via {opener}: {source}")]
    Open {
        opener: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("coulisse is not running — start it with `coulisse start`, then re-run this command")]
    ServerNotRunning,
}

/// # Errors
///
/// Returns an error if the config can't be read, the server isn't
/// running, or the OS-level browser opener fails to launch.
pub fn run(config_path: &Path) -> Result<(), StudioError> {
    let config = Config::from_path(config_path)?;
    let paths = StatePaths::for_config(config_path);
    let running = read_pid(&paths.pid).is_some_and(pid_alive);
    if !running {
        return Err(StudioError::ServerNotRunning);
    }
    let url = format!("http://localhost:{}/admin/", config.port.unwrap_or(8421));
    println!("opening {url}");
    open_in_browser(&url)
}

#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) -> Result<(), StudioError> {
    spawn("open", &[url])
}

#[cfg(target_os = "linux")]
fn open_in_browser(url: &str) -> Result<(), StudioError> {
    spawn("xdg-open", &[url])
}

#[cfg(target_os = "windows")]
fn open_in_browser(url: &str) -> Result<(), StudioError> {
    // WHY: `start` is a cmd builtin, not an exe — must go through cmd.
    // The empty "" argument is the window title; without it `start`
    // treats a quoted URL as the title and silently does nothing.
    spawn("cmd", &["/C", "start", "", url])
}

fn spawn(opener: &'static str, args: &[&str]) -> Result<(), StudioError> {
    Command::new(opener)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|source| StudioError::Open { opener, source })
}
