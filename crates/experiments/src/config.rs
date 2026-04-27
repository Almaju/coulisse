use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

/// One A/B test group. The `name` is addressable as a `model` value and
/// resolves to one of the `variants` per request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExperimentConfig {
    /// Bandit-only. Maximum age of scores included in mean-arm
    /// computations, in seconds. Older rows are ignored. Defaults to 7
    /// days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandit_window_seconds: Option<u64>,
    /// Bandit-only. Probability in `[0.0, 1.0]` of routing to a random
    /// arm instead of the leader. Defaults to `0.1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epsilon: Option<f32>,
    /// Bandit-only. `judge.criterion` to use as the optimisation
    /// metric. The named judge must declare the criterion in its
    /// rubrics, and every variant must opt into the judge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    /// Bandit-only. Each arm must accumulate at least this many scores
    /// before exploitation kicks in. Until then, the arm is forced.
    /// Defaults to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_samples: Option<u32>,
    /// Name clients address as `model`. Must not collide with any agent
    /// name and must not collide with another experiment name.
    pub name: String,
    /// Shadow-only. Required: the variant agent that serves the user.
    /// Other variants run in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    /// Optional tool description when this experiment is exposed via
    /// another agent's `subagents:`. Treated like an agent's `purpose`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Shadow-only. Probability in `[0.0, 1.0]` that any given turn
    /// also runs the non-primary variants in the background. Defaults
    /// to `1.0` (every turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_rate: Option<f32>,
    /// When true (the default), the same user always hits the same
    /// variant of this experiment. The mapping is a deterministic hash
    /// of `(user_id, experiment_name)` modulo the cumulative weights —
    /// no DB writes, stable across restarts. Adding or removing a
    /// variant reshuffles users. For bandit, sticky still applies
    /// per-decision, but mean scores update over time so a user may
    /// shift if a different arm overtakes the leader.
    #[serde(default = "default_sticky_by_user")]
    pub sticky_by_user: bool,
    pub strategy: Strategy,
    pub variants: Vec<Variant>,
}

fn default_sticky_by_user() -> bool {
    true
}

/// How requests are dispatched across an experiment's variants.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    /// Epsilon-greedy: read recent mean scores per arm, exploit the
    /// leader with `1 - epsilon` probability and explore otherwise.
    /// Arms with fewer than `min_samples` scores are forced (explored).
    Bandit,
    /// `primary` serves the user; the other variants run in the
    /// background and are scored. Cost-bounded by `sampling_rate`.
    Shadow,
    /// Weighted random sampling (sticky-by-user when enabled).
    Split,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Variant {
    /// Name of an agent declared under top-level `agents:`. Variants must
    /// reference concrete agents; nesting experiments is rejected.
    pub agent: String,
    /// Relative weight for `split`/`bandit` sampling. Must be > 0; the
    /// router normalises against the sum of all variant weights.
    #[serde(default = "default_variant_weight")]
    pub weight: f32,
}

fn default_variant_weight() -> f32 {
    1.0
}

/// Hot-reloadable list of experiment configs for admin display.
/// Routing (`ExperimentRouter`) currently still requires a restart to
/// pick up changes — admin sees the live YAML state, in-flight requests
/// keep their boot-time routing.
pub type ExperimentList = Arc<ArcSwap<Vec<ExperimentConfig>>>;

#[must_use]
pub fn experiment_list(initial: Vec<ExperimentConfig>) -> ExperimentList {
    Arc::new(ArcSwap::from_pointee(initial))
}
