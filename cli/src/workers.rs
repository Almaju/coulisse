//! Background worker pool that drains the `tasks` queue.
//!
//! Each worker polls `Tasks::next_runnable` and runs the claimed task
//! through the same `Agents::complete` path the sync HTTP handler uses.
//! Workers are stateless: how the task got enqueued (HTTP dispatch tool,
//! cron, webhook, sibling agent) is invisible at this layer.
//!
//! Workers are detached tokio tasks. They live until the process exits;
//! graceful shutdown of in-flight runs is a follow-up.

use std::sync::Arc;
use std::time::Duration;

use agents::{Agents, RigAgents};
use providers::{Message, Role};
use tasks::{Task, Tasks};
use tracing::{Instrument, error, info, info_span, warn};

/// Sleep this long between polls when the queue is empty. Short enough to
/// feel responsive, long enough to keep idle CPU near zero.
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Spawn `count` worker tokio tasks. The handles are detached — callers
/// hold no reference; workers exit when the runtime drops.
pub(crate) fn spawn(tasks: Arc<Tasks>, agents: Arc<RigAgents>, count: u32) {
    if count == 0 {
        return;
    }
    info!(workers = count, "task worker pool starting");
    for worker_id in 0..count {
        let tasks = Arc::clone(&tasks);
        let agents = Arc::clone(&agents);
        tokio::spawn(async move {
            worker_loop(worker_id, tasks, agents).await;
        });
    }
}

async fn worker_loop(worker_id: u32, tasks: Arc<Tasks>, agents: Arc<RigAgents>) {
    loop {
        match tasks.next_runnable().await {
            Err(err) => {
                error!(worker = worker_id, %err, "queue poll failed; backing off");
                tokio::time::sleep(IDLE_POLL_INTERVAL).await;
            }
            Ok(None) => {
                tokio::time::sleep(IDLE_POLL_INTERVAL).await;
            }
            Ok(Some(task)) => {
                run_one(worker_id, &tasks, &agents, task).await;
            }
        }
    }
}

async fn run_one(worker_id: u32, tasks: &Tasks, agents: &RigAgents, task: Task) {
    let span = info_span!(
        "task",
        task_id = %task.id.0,
        agent = %task.agent,
        worker = worker_id,
    );
    async move {
        info!("starting task");
        let messages = vec![Message {
            content: task.prompt.clone(),
            role: Role::User,
        }];
        match agents.complete(&task.agent, messages, task.user_id).await {
            Ok(completion) => {
                if let Err(e) = tasks.mark_done(task.id, &completion.text).await {
                    warn!(%e, "mark_done failed");
                } else {
                    info!("task done");
                }
            }
            Err(err) => {
                let msg = err.to_string();
                if let Err(e) = tasks.mark_errored(task.id, &msg).await {
                    warn!(%e, "mark_errored failed");
                }
                warn!(%msg, "task failed");
            }
        }
    }
    .instrument(span)
    .await;
}
