//! YAML-driven triggers that drop tasks on the queue without anyone making
//! an HTTP request.
//!
//! Boot, cron and webhook triggers all convert to the same shape — a task
//! enqueued via the `TaskQueue` trait from `coulisse-core` — so workers
//! draining the queue don't know or care how they were summoned.
//!
//! Decoupled from any specific chat tool by design. Boot and cron are
//! purely internal. Webhooks accept POSTs from anything that can speak
//! HTTP, so connecting Slack or GitHub or any other source is just running
//! a tiny bridge that POSTs to Coulisse.

mod boot;
mod config;
mod cron;
mod error;
mod webhook;

use std::sync::Arc;

use axum::Router;
use coulisse_core::{TaskQueue, UserId};

pub use config::{TriggerConfig, TriggerKind};
pub use cron::validate_all;
pub use error::TriggerError;

use boot::BootTrigger;
use cron::CronTrigger;
use webhook::WebhookTrigger;

/// Every entry declared under `triggers:`, bound to the queue its tasks
/// land on and the user the tasks run as.
pub struct Triggers {
    configs: Vec<TriggerConfig>,
    queue: Arc<dyn TaskQueue>,
    user_id: UserId,
}

impl Triggers {
    #[must_use]
    pub fn new(configs: &[TriggerConfig], queue: Arc<dyn TaskQueue>, user_id: UserId) -> Self {
        Self {
            configs: configs.to_vec(),
            queue,
            user_id,
        }
    }

    /// Submit one task per `boot` trigger to the queue. Returns
    /// immediately; workers pick the tasks up like any other.
    ///
    /// Non-boot variants are ignored — they're handled by `spawn_cron` or
    /// `webhook_router`.
    pub async fn fire_boot(&self) {
        for trigger in self
            .configs
            .iter()
            .filter_map(|config| BootTrigger::from_config(config, self.user_id))
        {
            trigger.fire(self.queue.as_ref()).await;
        }
    }

    /// Spawn one tokio task per cron trigger. Each task sleeps until the
    /// next scheduled fire, enqueues a task via the `TaskQueue` trait,
    /// repeats.
    ///
    /// Webhook triggers are ignored; they're served by `webhook_router`
    /// instead. Callers should have called `validate_all` first; this
    /// method silently skips triggers with unparseable schedules to keep
    /// the runtime crash-free, but the boot-time validator is the right
    /// place to surface those errors.
    pub fn spawn_cron(&self) {
        for trigger in self
            .configs
            .iter()
            .filter_map(|config| CronTrigger::from_config(config, self.user_id))
        {
            trigger.spawn(Arc::clone(&self.queue));
        }
    }

    /// Build an axum router that mounts one `POST` handler per webhook
    /// trigger. Non-webhook entries (boot, cron) are ignored.
    ///
    /// The returned router uses the unit state `()`; each handler holds
    /// its own per-trigger state baked in.
    pub fn webhook_router(&self) -> Router {
        self.configs
            .iter()
            .filter_map(|config| {
                WebhookTrigger::from_config(config, Arc::clone(&self.queue), self.user_id)
            })
            .fold(Router::new(), |router, trigger| trigger.mount(router))
    }
}
