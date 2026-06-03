#![allow(unsafe_code)]

//! `coulisse start` — run the server, detached by default.
//!
//! Detach is implemented by self-respawn: spawn `coulisse start
//! --foreground` as a child with stdio redirected to the log file and
//! `setsid` so it survives the parent. The child writes the PID file
//! once it's safely set up; the parent waits for that file (or a
//! timeout) before returning so callers see a deterministic ready/fail.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use nix::unistd::setsid;

use crate::commands::serve;
use crate::commands::status::pid_alive;
use crate::paths::StatePaths;

const DETACH_FLAG: &str = "--detached-child";
const READY_FILENAME: &str = "ready";
// Boot can include sqlite migrations, MCP handshakes, and store rebuilds
// before the listener binds; pick a deadline that comfortably covers a
// cold start so we don't false-timeout a legitimate boot.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("coulisse already running (pid {0}) — run `coulisse stop` first")]
    AlreadyRunning(i32),
    #[error(
        "server exited during startup ({status})\n  log: {log_path}\n--- server output ---\n{tail}"
    )]
    ChildExited {
        log_path: String,
        status: ExitStatus,
        tail: String,
    },
    #[error("config not found at {0} — run `coulisse init` first")]
    ConfigMissing(String),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Serve(Box<dyn std::error::Error>),
    #[error(
        "server failed to come up within {duration:?}\n  log: {log_path}\n--- recent server output ---\n{tail}"
    )]
    StartTimeout {
        duration: Duration,
        log_path: String,
        tail: String,
    },
}

pub struct Options {
    pub detached_child: bool,
    pub foreground: bool,
}

/// # Errors
///
/// Returns an error if the underlying operation fails.
pub fn run(config_path: &Path, opts: &Options) -> Result<(), StartError> {
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
    write_pid(&paths.pid, current_pid())?;
    let result = serve_blocking(config_path, || {});
    let _ = fs::remove_file(&paths.pid);
    result
}

/// Variant of foreground that runs after self-respawn: the parent has
/// already redirected stdio and we just need to write the pid and serve.
/// The `on_ready` callback (a touch on the `ready` marker file) is how
/// the launching parent knows the server has actually bound its port.
fn run_as_detached_child(config_path: &Path, paths: &StatePaths) -> Result<(), StartError> {
    write_pid(&paths.pid, current_pid())?;
    let ready = paths.dir.join(READY_FILENAME);
    let ready_signal = ready.clone();
    let result = serve_blocking(config_path, move || {
        let _ = File::create(&ready_signal);
    });
    let _ = fs::remove_file(&paths.pid);
    let _ = fs::remove_file(&ready);
    result
}

fn serve_blocking(config_path: &Path, on_ready: impl FnOnce() + Send) -> Result<(), StartError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartError::Io)?;
    runtime
        .block_on(serve::run(config_path, on_ready))
        .map_err(StartError::Serve)
}

fn spawn_detached(config_path: &Path, paths: &StatePaths) -> Result<(), StartError> {
    if let Some(pid) = read_pid(&paths.pid)
        && pid_alive(pid)
    {
        return Err(StartError::AlreadyRunning(pid));
    }
    // NOTE: stale state from a previous crash — replace both files so
    // we never observe a pre-existing ready marker and false-positive.
    let _ = fs::remove_file(&paths.pid);
    let ready = paths.dir.join(READY_FILENAME);
    let _ = fs::remove_file(&ready);

    fs::create_dir_all(&paths.dir)?;
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&paths.log)?;
    // WHY: record the log size before spawn so on failure we can surface
    // only this run's output, not noise from previous runs.
    let log_offset = log.metadata().map_or(0, |m| m.len());
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
            // WHY: detach from controlling terminal so SIGHUP from the
            // launching shell doesn't kill the server.
            setsid().map_err(io::Error::from)?;
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let child_pid = i32::try_from(child.id()).unwrap_or(i32::MAX);

    let deadline = Instant::now() + READY_TIMEOUT;
    // Wait for the child to touch the ready marker, which it only does
    // after `TcpListener::bind` returns inside `serve::run`. Anything
    // that fails before bind — config parse, sqlite open, port-in-use —
    // exits the child with no marker, and `try_wait` surfaces the log.
    loop {
        if ready.exists() {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(StartError::ChildExited {
                log_path: paths.log.display().to_string(),
                status,
                tail: read_log_since(&paths.log, log_offset),
            });
        }
        if Instant::now() >= deadline {
            return Err(StartError::StartTimeout {
                duration: READY_TIMEOUT,
                log_path: paths.log.display().to_string(),
                tail: read_log_since(&paths.log, log_offset),
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // WHY: leak the child handle so we don't try to wait/kill on drop.
    drop(child);

    println!("coulisse started (pid {child_pid})");
    println!("  config: {}", config_path.display());
    println!("  log:    {}", paths.log.display());
    println!("  stop with: coulisse stop");
    Ok(())
}

/// Read the log file from `start` to EOF, return the last ~30 lines
/// trimmed. Best-effort: returns an empty string if the file can't be
/// read.
fn read_log_since(path: &Path, start: u64) -> String {
    const MAX_LINES: usize = 30;
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return String::new();
    }
    let trimmed = buf.trim();
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() <= MAX_LINES {
        return trimmed.to_string();
    }
    lines[lines.len() - MAX_LINES..].join("\n")
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

/// `std::process::id()` returns u32; nix's `Pid::from_raw` and our pid
/// file format use i32. Real PIDs always fit (Linux caps at 2^22).
fn current_pid() -> i32 {
    i32::try_from(std::process::id()).unwrap_or(i32::MAX)
}
