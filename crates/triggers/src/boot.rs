//! Boot trigger — fires exactly once when Coulisse starts.
//!
//! Same submission path as cron and webhook: each `type: boot` entry under
//! `triggers:` enqueues one task on the queue at startup, then exits. Use
//! cases are wake-up prompts that should run on every `coulisse start` —
//! e.g. asking the orchestrator agent to read the queue's leftovers and
//! decide whether a standup is warranted, without forcing a ritual on
//! every restart.

use coulisse_core::{TaskQueue, TaskSubmission, UserId};
use tracing::{error, info};

use crate::config::{TriggerConfig, TriggerKind};

pub(crate) struct BootTrigger {
    agent: String,
    name: String,
    prompt: String,
    user_id: UserId,
}

impl BootTrigger {
    /// `None` when `config` is not a `boot` trigger.
    pub(crate) fn from_config(config: &TriggerConfig, user_id: UserId) -> Option<Self> {
        let TriggerKind::Boot {} = &config.kind else {
            return None;
        };
        Some(Self {
            agent: config.agent.clone(),
            name: config.name.clone(),
            prompt: config.prompt.clone(),
            user_id,
        })
    }

    pub(crate) async fn fire(&self, queue: &dyn TaskQueue) {
        let submission = TaskSubmission {
            agent: &self.agent,
            prompt: &self.prompt,
            user_id: self.user_id,
        };
        match queue.submit(submission).await {
            Ok(task_id) => {
                info!(
                    trigger = %self.name,
                    agent = %self.agent,
                    task_id = %task_id.0,
                    "boot trigger fired",
                );
            }
            Err(e) => {
                error!(trigger = %self.name, %e, "boot trigger failed to enqueue");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Triggers;
    use coulisse_core::{BoxFuture, TaskId, TaskQueueError};
    use std::sync::{Arc, Mutex};

    struct CapturingQueue {
        calls: Mutex<Vec<(String, String)>>,
    }

    impl TaskQueue for CapturingQueue {
        fn submit<'a>(
            &'a self,
            submission: TaskSubmission<'a>,
        ) -> BoxFuture<'a, Result<TaskId, TaskQueueError>> {
            self.calls
                .lock()
                .unwrap()
                .push((submission.agent.to_string(), submission.prompt.to_string()));
            Box::pin(async { Ok(TaskId::new()) })
        }
    }

    #[tokio::test]
    async fn fires_boot_entries_and_skips_others() {
        let triggers = vec![
            TriggerConfig {
                agent: "pm".into(),
                kind: TriggerKind::Boot {},
                name: "wakeup".into(),
                prompt: "wake up".into(),
            },
            TriggerConfig {
                agent: "coder".into(),
                kind: TriggerKind::Cron {
                    schedule: "0 9 * * *".into(),
                },
                name: "morning".into(),
                prompt: "should be ignored".into(),
            },
        ];
        let queue = Arc::new(CapturingQueue {
            calls: Mutex::new(Vec::new()),
        });
        Triggers::new(&triggers, queue.clone(), UserId::new())
            .fire_boot()
            .await;
        let calls = queue.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "pm");
        assert_eq!(calls[0].1, "wake up");
    }

    #[test]
    fn boot_variant_deserializes_from_minimal_yaml() {
        let yaml = "agent: pm\nname: wakeup\nprompt: hi\ntype: boot\n";
        let parsed: TriggerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(parsed.kind, TriggerKind::Boot {}));
    }
}
