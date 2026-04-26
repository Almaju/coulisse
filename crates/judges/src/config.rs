use std::collections::BTreeMap;

use serde::Deserialize;

/// Runtime config for one LLM-as-judge evaluator. A judge runs in a
/// background task after each assistant turn of agents that reference it,
/// sampling at `sampling_rate`, and produces one `Score` row per criterion
/// in `rubrics`.
///
/// The user only describes *what* to evaluate; Coulisse builds the judge
/// preamble and forces JSON output internally — users should not write scale
/// or format instructions into their rubrics.
#[derive(Clone, Debug, Deserialize)]
pub struct JudgeConfig {
    pub model: String,
    pub name: String,
    pub provider: String,
    /// Map of criterion name → short description of what to assess. Each
    /// criterion produces one score per scored turn. `BTreeMap` gives
    /// deterministic, alphabetical order in the judge preamble.
    #[serde(default)]
    pub rubrics: BTreeMap<String, String>,
    /// Probability in [0, 1] that any given assistant turn is scored.
    /// 1.0 = every turn, 0.1 = ~10% of turns. Defaults to 1.0.
    #[serde(default = "default_sampling_rate")]
    pub sampling_rate: f32,
}

fn default_sampling_rate() -> f32 {
    1.0
}
