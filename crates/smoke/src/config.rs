use std::sync::Arc;

use arc_swap::{ArcSwap, Guard};
use serde::{Deserialize, Serialize};

/// One smoke test: a synthetic-user persona that drives a conversation
/// against an agent (or experiment) for evaluation. Each repetition uses
/// a fresh synthetic `user_id`, so split/bandit experiments sample variants
/// naturally across reps.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct SmokeTestConfig {
    /// Optional opening message from the synthetic user. When omitted,
    /// the persona produces the first turn itself from its preamble.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<String>,
    /// Hard ceiling on persona-then-agent turn pairs. Stops the loop
    /// even if no `stop_marker` appears.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    pub name: String,
    pub persona: PersonaConfig,
    /// How many independent runs to launch when the test is triggered.
    /// Each run gets its own synthetic `user_id`.
    #[serde(default = "default_repetitions")]
    pub repetitions: u32,
    /// If either side emits this substring, the run terminates after
    /// recording that turn. Useful for personas that signal "I have what
    /// I needed" with a sentinel like `[FIN]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_marker: Option<String>,
    /// Agent or experiment name to evaluate. Resolved through the
    /// experiment router at run time, so a target of `"my_exp"` will
    /// pick a variant per run.
    pub target: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct PersonaConfig {
    pub model: String,
    pub preamble: String,
    pub provider: String,
}

fn default_max_turns() -> u32 {
    10
}

fn default_repetitions() -> u32 {
    1
}

/// Hot-reloadable list of smoke tests. Same `ArcSwap` shape used by
/// the other feature crates: clones share the underlying list, so cli
/// can hand one copy to the reload watcher and another to the admin
/// router.
#[derive(Clone, Debug)]
pub struct SmokeList(Arc<ArcSwap<Vec<SmokeTestConfig>>>);

impl SmokeList {
    #[must_use]
    pub fn new(initial: Vec<SmokeTestConfig>) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(initial)))
    }

    #[must_use]
    pub fn load(&self) -> Guard<Arc<Vec<SmokeTestConfig>>> {
        self.0.load()
    }

    #[must_use]
    pub fn load_full(&self) -> Arc<Vec<SmokeTestConfig>> {
        self.0.load_full()
    }

    pub fn store(&self, configs: Arc<Vec<SmokeTestConfig>>) {
        self.0.store(configs);
    }
}
