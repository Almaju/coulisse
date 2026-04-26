//! `coulisse start` — run the server, detached by default.
//!
//! Detach is implemented by self-respawn: spawn `coulisse start
//! --foreground` as a child with stdio redirected to the log file and
//! `setsid` so it survives the parent. The child writes the PID file
//! once it's safely set up; the parent waits for that file (or a
//! timeout) before returning so callers see a deterministic ready/fail.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nix::unistd::setsid;

use crate::commands::serve;
use crate::commands::status::pid_alive;
use crate::paths::StatePaths;

const DETACH_FLAG: &str = "--detached-child";
const READY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("config not found at {0} — run `coulisse init` first")]
    ConfigMissing(String),
    #[error("coulisse already running (pid {0}) — run `coulisse stop` first")]
    AlreadyRunning(i32),
    #[error("server failed to come up within {0:?} — see {1}")]
    StartTimeout(Duration, String),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Serve(Box<dyn std::error::Error>),
}

pub struct Options {
    pub detached_child: bool,
    pub foreground: bool,
}

pub fn run(config_path: &Path, opts: Options) -> Result<(), StartError> {
    if !config_path.exists() {
        return Err(StartError::ConfigMissing(config_path.display().to_string()));
    }
    let paths = StatePaths::for_config(config_path);

    if opts.detached_child {
        return run_as_detached_child(config_path, &paths);
    }
    if opts.foreground {
        return run_foreground(config_path, &paths);
    }
    spawn_detached(config_path, &paths)
}

fn run_foreground(config_path: &Path, paths: &StatePaths) -> Result<(), StartError> {
    if let Some(pid) = read_pid(&paths.pid)
        && pid_alive(pid)
    {
        return Err(StartError::AlreadyRunning(pid));
    }
    fs::create_dir_all(&paths.dir)?;
    write_pid(&paths.pid, std::process::id() as i32)?;
    let result = serve_blocking(config_path);
    let _ = fs::remove_file(&paths.pid);
    result
}

/// Variant of foreground that runs after self-respawn: the parent has
/// already redirected stdio and we just need to write the pid and serve.
fn run_as_detached_child(config_path: &Path, paths: &StatePaths) -> Result<(), StartError> {
    write_pid(&paths.pid, std::process::id() as i32)?;
    let result = serve_blocking(config_path);
    let _ = fs::remove_file(&paths.pid);
    result
}

fn serve_blocking(config_path: &Path) -> Result<(), StartError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartError::Io)?;
    runtime
        .block_on(serve::run(config_path))
        .map_err(StartError::Serve)
}

fn spawn_detached(config_path: &Path, paths: &StatePaths) -> Result<(), StartError> {
    if let Some(pid) = read_pid(&paths.pid)
        && pid_alive(pid)
    {
        return Err(StartError::AlreadyRunning(pid));
    }
    // Stale PID file from a previous crash — replace it.
    let _ = fs::remove_file(&paths.pid);

    fs::create_dir_all(&paths.dir)?;
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&paths.log)?;
    let log_err = log.try_clone()?;

    let exe = env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("start")
        .arg("--config")
        .arg(config_path)
        .arg(DETACH_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    unsafe {
        cmd.pre_exec(|| {
            // Detach from controlling terminal so SIGHUP from the
            // launching shell doesn't kill the server.
            setsid().map_err(io::Error::from)?;
            Ok(())
        });
    }
    let child = cmd.spawn()?;

    let deadline = Instant::now() + READY_TIMEOUT;
    let pid = loop {
        if let Some(pid) = read_pid(&paths.pid)
            && pid_alive(pid)
        {
            break pid;
        }
        if Instant::now() >= deadline {
            return Err(StartError::StartTimeout(
                READY_TIMEOUT,
                paths.log.display().to_string(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Detach: leak the child handle so we don't try to wait/kill on drop.
    drop(child);

    println!("coulisse started (pid {pid})");
    println!("  config: {}", config_path.display());
    println!("  log:    {}", paths.log.display());
    println!("  stop with: coulisse stop");
    Ok(())
}

fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_pid(path: &Path, pid: i32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = File::create(path)?;
    writeln!(f, "{pid}")
}
