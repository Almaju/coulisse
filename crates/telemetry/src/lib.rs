//! Operator-visibility storage for Coulisse. Captures what happened during a
//! turn (tool calls, subagent calls, LLM round-trips) into an `events` table
//! that the studio UI renders as a causal tree. Deliberately separate from
//! `memory` so observability growth never risks leaking into the prompt.

mod ctx;
mod error;
mod event;
mod id;
mod sink;

pub use coulisse_core::TurnId;
pub use ctx::Ctx;
pub use error::TelemetryError;
pub use event::{Event, EventKind};
pub use id::EventId;
pub use sink::Sink;
