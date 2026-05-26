//! YAML-driven triggers that drop tasks on the queue without anyone making
//! an HTTP request.
//!
//! Today: cron. Coming: webhooks. Both convert to the same shape — a task
//! enqueued via the `TaskQueue` trait from `coulisse-core` — so workers
//! draining the queue don't know or care how they were summoned.
//!
//! Decoupled from any specific chat tool by design. Cron is purely
//! internal. Webhooks (next slice) will accept POSTs from anything that
//! can speak HTTP, so connecting Matrix or Slack or GitHub is just running
//! a tiny bridge that POSTs to Coulisse.

mod config;
mod cron;
mod error;
mod webhook;

pub use config::{TriggerConfig, TriggerKind};
pub use cron::{spawn_cron, validate_all};
pub use error::TriggerError;
pub use webhook::webhook_router;
