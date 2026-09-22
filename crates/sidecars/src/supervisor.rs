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
            cfg.supervise().await;
        });
    }
}

impl SidecarConfig {
    async fn supervise(self) {
        loop {
            info!(
                sidecar = %self.name,
                command = %self.command,
                "spawning sidecar",
            );
            let mut builder = Command::new(&self.command);
            builder
                .args(&self.args)
                .envs(&self.env)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(cwd) = &self.cwd {
                builder.current_dir(cwd);
            }
            let mut child = match builder.spawn() {
                Ok(c) => c,
                Err(e) => {
                    error!(
                        sidecar = %self.name,
                        %e,
                        "sidecar spawn failed",
                    );
                    if !self.restart.should_restart(None) {
                        return;
                    }
                    tokio::time::sleep(RESTART_BACKOFF).await;
                    continue;
                }
            };

            if let Some(stdout) = child.stdout.take() {
                let name = self.name.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        info!(sidecar = %name, stream = "stdout", "{}", line);
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let name = self.name.clone();
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
                    error!(sidecar = %self.name, %e, "wait failed");
                    if !self.restart.should_restart(None) {
                        return;
                    }
                    tokio::time::sleep(RESTART_BACKOFF).await;
                    continue;
                }
            };

            let success = status.success();
            info!(
                sidecar = %self.name,
                success,
                code = ?status.code(),
                "sidecar exited",
            );
            if !self.restart.should_restart(Some(success)) {
                return;
            }
            tokio::time::sleep(RESTART_BACKOFF).await;
        }
    }
}

impl RestartPolicy {
    fn should_restart(self, exit_success: Option<bool>) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::OnFailure => exit_success != Some(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_does_not_restart() {
        assert!(!RestartPolicy::Never.should_restart(Some(true)));
        assert!(!RestartPolicy::Never.should_restart(Some(false)));
        assert!(!RestartPolicy::Never.should_restart(None));
    }

    #[test]
    fn always_always_restarts() {
        assert!(RestartPolicy::Always.should_restart(Some(true)));
        assert!(RestartPolicy::Always.should_restart(Some(false)));
        assert!(RestartPolicy::Always.should_restart(None));
    }

    #[test]
    fn on_failure_skips_clean_exit() {
        assert!(!RestartPolicy::OnFailure.should_restart(Some(true)));
        assert!(RestartPolicy::OnFailure.should_restart(Some(false)));
        assert!(RestartPolicy::OnFailure.should_restart(None));
    }
}
