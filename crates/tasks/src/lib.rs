//! Background agent task queue.
//!
//! Owns the lifecycle of fire-and-forget agent runs: `queued → running →
//! done | errored`. Agents submit work via the `TaskQueue` trait in
//! `coulisse-core`; a worker pool in `cli` drains the queue and runs each
//! task through the same handler as the sync `/v1/chat/completions`
//! endpoint. Workers don't know how they were triggered — cron, webhook, a
//! sibling agent, or an HTTP request — only that something asked for an
//! agent to run with a given prompt.
//!
//! Each persistent feature crate owns its own table; `tasks` owns the
//! `tasks` table. Reads from outside this crate go through the
//! `TaskQueue` trait so siblings don't see the storage layer.
//!
//! Does **not** depend on `agents`. The crate is purely a queue and a state
//! machine; the cli wires the dispatch path between this crate and the
//! agents runtime.

mod error;
mod queue;

pub use error::TaskError;
pub use queue::{Task, TaskState, Tasks};
