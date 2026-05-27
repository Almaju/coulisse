//! Boot trigger — fires exactly once when Coulisse starts.
//!
//! Same submission path as cron and webhook: each `type: boot` entry under
//! `triggers:` enqueues one task on the queue at startup, then exits. Use
//! cases are wake-up prompts that should run on every `coulisse start` —
//! e.g. asking the orchestrator agent to read the queue's leftovers and
//! decide whether a standup is warranted, without forcing a ritual on
//! every restart.

use std::sync::Arc;

use coulisse_core::{TaskQueue, UserId};
use tracing::{error, info};

use crate::config::{TriggerConfig, TriggerKind};

/// Submit one task per `boot` trigger to the queue. Returns immediately;
/// workers pick the tasks up like any other.
///
/// Non-boot variants are ignored — they're handled by `spawn_cron` or
/// `webhook_router`.
pub async fn fire_boot(triggers: &[TriggerConfig], queue: Arc<dyn TaskQueue>, user_id: UserId) {
    for trigger in triggers {
        let TriggerKind::Boot {} = &trigger.kind else {
            continue;
        };
        match queue.submit(&trigger.agent, &trigger.prompt, user_id).await {
            Ok(task_id) => {
                info!(
                    trigger = %trigger.name,
                    agent = %trigger.agent,
                    task_id = %task_id.0,
                    "boot trigger fired",
                );
            }
            Err(e) => {
                error!(trigger = %trigger.name, %e, "boot trigger failed to enqueue");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coulisse_core::{TaskId, TaskQueueError};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    struct CapturingQueue {
        calls: Mutex<Vec<(String, String)>>,
    }

    impl TaskQueue for CapturingQueue {
        fn submit<'a>(
            &'a self,
            agent: &'a str,
            prompt: &'a str,
            _user_id: UserId,
        ) -> Pin<Box<dyn Future<Output = Result<TaskId, TaskQueueError>> + Send + 'a>> {
            self.calls
                .lock()
                .unwrap()
                .push((agent.to_string(), prompt.to_string()));
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
        fire_boot(&triggers, queue.clone(), UserId::new()).await;
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
