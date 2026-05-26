use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{error, info, warn};

use crate::config::{RestartPolicy, SidecarConfig};

/// Backoff between restart attempts. Cheap, fixed; we don't need
/// exponential backoff for sidecars yet.
const RESTART_BACKOFF: Duration = Duration::from_secs(2);

/// Spawn one supervisor tokio task per sidecar. Each supervises its own
/// child end-to-end (spawn, capture output, wait, restart per policy).
/// Tasks are detached — they live until the process exits.
pub fn spawn_all(sidecars: &[SidecarConfig]) {
    if sidecars.is_empty() {
        return;
    }
    info!(count = sidecars.len(), "sidecars starting");
    for cfg in sidecars {
        let cfg = cfg.clone();
        tokio::spawn(async move {
            supervise(cfg).await;
        });
    }
}

async fn supervise(cfg: SidecarConfig) {
    loop {
        info!(
            sidecar = %cfg.name,
            command = %cfg.command,
            "spawning sidecar",
        );
        let mut builder = Command::new(&cfg.command);
        builder
            .args(&cfg.args)
            .envs(&cfg.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &cfg.cwd {
            builder.current_dir(cwd);
        }
        let mut child = match builder.spawn() {
            Ok(c) => c,
            Err(e) => {
                error!(
                    sidecar = %cfg.name,
                    %e,
                    "sidecar spawn failed",
                );
                if !should_restart(cfg.restart, None) {
                    return;
                }
                tokio::time::sleep(RESTART_BACKOFF).await;
                continue;
            }
        };

        if let Some(stdout) = child.stdout.take() {
            let name = cfg.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    info!(sidecar = %name, stream = "stdout", "{}", line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let name = cfg.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    warn!(sidecar = %name, stream = "stderr", "{}", line);
                }
            });
        }

        let status = match child.wait().await {
            Ok(s) => s,
            Err(e) => {
                error!(sidecar = %cfg.name, %e, "wait failed");
                if !should_restart(cfg.restart, None) {
                    return;
                }
                tokio::time::sleep(RESTART_BACKOFF).await;
                continue;
            }
        };

        let success = status.success();
        info!(
            sidecar = %cfg.name,
            success,
            code = ?status.code(),
            "sidecar exited",
        );
        if !should_restart(cfg.restart, Some(success)) {
            return;
        }
        tokio::time::sleep(RESTART_BACKOFF).await;
    }
}

fn should_restart(policy: RestartPolicy, exit_success: Option<bool>) -> bool {
    match policy {
        RestartPolicy::Always => true,
        RestartPolicy::Never => false,
        RestartPolicy::OnFailure => exit_success != Some(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_does_not_restart() {
        assert!(!should_restart(RestartPolicy::Never, Some(true)));
        assert!(!should_restart(RestartPolicy::Never, Some(false)));
        assert!(!should_restart(RestartPolicy::Never, None));
    }

    #[test]
    fn always_always_restarts() {
        assert!(should_restart(RestartPolicy::Always, Some(true)));
        assert!(should_restart(RestartPolicy::Always, Some(false)));
        assert!(should_restart(RestartPolicy::Always, None));
    }

    #[test]
    fn on_failure_skips_clean_exit() {
        assert!(!should_restart(RestartPolicy::OnFailure, Some(true)));
        assert!(should_restart(RestartPolicy::OnFailure, Some(false)));
        assert!(should_restart(RestartPolicy::OnFailure, None));
    }
}
