use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One `TurnId` per user-facing request — the public-visible correlation id.
/// All events emitted while processing that request share this id, regardless
/// of how deep the subagent tree goes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TurnId(pub Uuid);

impl TurnId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TurnId {
    fn default() -> Self {
        Self::new()
    }
}

/// One `EventId` per emitted row. An event's `parent_id` — itself an
/// `EventId` — nests it under the scope that triggered it. The tree rooted
/// at the top-level `turn_start` event captures the full causal structure
/// of a turn, including nested subagent recursion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}
