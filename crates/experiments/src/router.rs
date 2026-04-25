use std::borrow::Cow;
use std::collections::HashMap;

use coulisse_core::{AgentScoreSummary, UserId};

use crate::{ExperimentConfig, Strategy, Variant};

/// Default exploration probability for the bandit strategy when not
/// overridden in YAML. 10% is a common pragma — enough to surface a
/// new arm overtaking the leader without burning too many requests on
/// known-worse variants.
pub const BANDIT_DEFAULT_EPSILON: f32 = 0.1;
/// Default minimum-samples-per-arm threshold. Until each arm crosses
/// this, exploitation is forced off and the bandit picks among the
/// arms still building up evidence. 30 is small enough to converge on
/// realistic traffic and large enough to dampen noise.
pub const BANDIT_DEFAULT_MIN_SAMPLES: u32 = 30;
/// Default lookback window for bandit mean-score queries: seven days.
/// Long enough to span typical evaluation noise, short enough that a
/// regression in production scoring shifts the leader.
pub const BANDIT_DEFAULT_WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Outcome of resolving a request's `model` (or a subagent name) against
/// the experiment table. `experiment` is `Some(name)` when the input was
/// an experiment — useful for telemetry and the studio.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved<'a> {
    pub agent: Cow<'a, str>,
    pub experiment: Option<&'a str>,
}

/// Lookup table over the experiments declared in `coulisse.yaml`. Cheap
/// to clone via `Arc` if you need to pass it across tasks; the typical
/// caller holds it inside the prompter and never mutates it.
pub struct ExperimentRouter {
    by_name: HashMap<String, ExperimentConfig>,
}

impl ExperimentRouter {
    pub fn new(experiments: Vec<ExperimentConfig>) -> Self {
        let by_name = experiments
            .into_iter()
            .map(|exp| (exp.name.clone(), exp))
            .collect();
        Self { by_name }
    }

    pub fn experiments(&self) -> impl Iterator<Item = &ExperimentConfig> {
        self.by_name.values()
    }

    pub fn get(&self, name: &str) -> Option<&ExperimentConfig> {
        self.by_name.get(name)
    }

    /// For a bandit experiment, return the score-query inputs the
    /// caller needs to fetch from memory before resolving:
    /// `(judge, criterion, since_seconds)`. Returns `None` for
    /// non-bandit strategies, which don't read scores.
    pub fn bandit_query(&self, name: &str) -> Option<(String, String, u64)> {
        let exp = self.by_name.get(name)?;
        if !matches!(exp.strategy, Strategy::Bandit) {
            return None;
        }
        let metric = exp.metric.as_deref()?;
        let (judge, criterion) = metric.split_once('.')?;
        let window = exp
            .bandit_window_seconds
            .unwrap_or(BANDIT_DEFAULT_WINDOW_SECONDS);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let since = now.saturating_sub(window);
        Some((judge.to_string(), criterion.to_string(), since))
    }

    /// Resolve `name` to a concrete agent for `user_id`. If `name` is
    /// not an experiment, it's returned unchanged — callers don't need
    /// to know in advance whether they're addressing an experiment or
    /// an agent.
    ///
    /// Bandit experiments without scores fall back to a weighted hash
    /// pick (effectively `split`). Callers that have score data should
    /// use `resolve_with_scores` to get the bandit decision.
    pub fn resolve<'a>(&'a self, name: &'a str, user_id: UserId) -> Resolved<'a> {
        self.resolve_with_scores(name, user_id, &[])
    }

    /// Like `resolve`, but `scores` participates in bandit decisions —
    /// each entry is the recent mean for a candidate variant agent.
    /// For non-bandit strategies the slice is ignored, so callers may
    /// pass an empty slice when they don't have data on hand.
    pub fn resolve_with_scores<'a>(
        &'a self,
        name: &'a str,
        user_id: UserId,
        scores: &[AgentScoreSummary],
    ) -> Resolved<'a> {
        match self.by_name.get(name) {
            Some(experiment) => {
                let variant = pick_variant(experiment, user_id, scores);
                Resolved {
                    agent: Cow::Owned(variant.agent.clone()),
                    experiment: Some(experiment.name.as_str()),
                }
            }
            None => Resolved {
                agent: Cow::Borrowed(name),
                experiment: None,
            },
        }
    }

    /// Variants other than the shadow primary, in declaration order.
    /// Returns an empty slice for non-shadow experiments so callers
    /// can blindly iterate without strategy-specific guards.
    pub fn shadow_variants<'a>(
        &'a self,
        experiment: &'a ExperimentConfig,
    ) -> impl Iterator<Item = &'a Variant> + 'a {
        let primary = match experiment.strategy {
            Strategy::Shadow => experiment.primary.as_deref(),
            _ => None,
        };
        experiment
            .variants
            .iter()
            .filter(move |v| Some(v.agent.as_str()) != primary)
    }

    /// True iff a shadow experiment should also run its non-primary
    /// variants for this turn. Always `true` for non-shadow strategies
    /// (callers gate that themselves) — shadow gates probabilistically
    /// based on `sampling_rate`.
    pub fn shadow_should_sample(&self, experiment: &ExperimentConfig, user_id: UserId) -> bool {
        if !matches!(experiment.strategy, Strategy::Shadow) {
            return false;
        }
        let rate = experiment.sampling_rate.unwrap_or(1.0);
        if rate >= 1.0 {
            return true;
        }
        if rate <= 0.0 {
            return false;
        }
        // Hash a per-turn seed (user + experiment + nanoseconds) and
        // compare against the rate. Avoids pulling in `rand` for what
        // is effectively a coin flip on the request hot path.
        let seed = per_request_seed(user_id, &experiment.name);
        let bucket = (seed as f64 / u64::MAX as f64) as f32;
        bucket < rate
    }
}

fn pick_variant<'a>(
    experiment: &'a ExperimentConfig,
    user_id: UserId,
    scores: &[AgentScoreSummary],
) -> &'a Variant {
    match experiment.strategy {
        Strategy::Split => weighted_pick(experiment, user_id),
        Strategy::Shadow => shadow_pick(experiment),
        Strategy::Bandit => bandit_pick(experiment, user_id, scores),
    }
}

fn shadow_pick(experiment: &ExperimentConfig) -> &Variant {
    // Validation guarantees `primary` is present and references one of
    // the variants for shadow strategy.
    let primary = experiment
        .primary
        .as_deref()
        .expect("validation guarantees shadow has primary");
    experiment
        .variants
        .iter()
        .find(|v| v.agent == primary)
        .expect("validation guarantees primary is a variant")
}

/// Epsilon-greedy bandit pick. Arms below `min_samples` are forced; if
/// any arm is forced, exploration consumes that turn. Otherwise with
/// probability `epsilon` we pick a hash-stable random arm, else the
/// arm with the highest mean. Ties go to the first arm by declaration
/// order, which makes the choice deterministic on ties.
fn bandit_pick<'a>(
    experiment: &'a ExperimentConfig,
    user_id: UserId,
    scores: &[AgentScoreSummary],
) -> &'a Variant {
    let min_samples = experiment.min_samples.unwrap_or(BANDIT_DEFAULT_MIN_SAMPLES);
    let forced: Vec<&Variant> = experiment
        .variants
        .iter()
        .filter(|v| {
            scores
                .iter()
                .find(|s| s.agent_name == v.agent)
                .map(|s| s.samples < min_samples)
                .unwrap_or(true)
        })
        .collect();
    if !forced.is_empty() {
        return uniform_hash_pick(&forced, user_id, &experiment.name);
    }
    let epsilon = experiment.epsilon.unwrap_or(BANDIT_DEFAULT_EPSILON);
    let seed = if experiment.sticky_by_user {
        sticky_seed(user_id, &experiment.name)
    } else {
        per_request_seed(user_id, &experiment.name)
    };
    let bucket = (seed as f64 / u64::MAX as f64) as f32;
    if bucket < epsilon {
        let arms: Vec<&Variant> = experiment.variants.iter().collect();
        return uniform_hash_pick(&arms, user_id, &experiment.name);
    }
    experiment
        .variants
        .iter()
        .max_by(|a, b| {
            let mean_a = scores
                .iter()
                .find(|s| s.agent_name == a.agent)
                .map(|s| s.mean)
                .unwrap_or(f32::MIN);
            let mean_b = scores
                .iter()
                .find(|s| s.agent_name == b.agent)
                .map(|s| s.mean)
                .unwrap_or(f32::MIN);
            mean_a
                .partial_cmp(&mean_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("validation rejects experiments with no variants")
}

fn uniform_hash_pick<'a>(arms: &[&'a Variant], user_id: UserId, name: &str) -> &'a Variant {
    let seed = sticky_seed(user_id, name);
    let idx = (seed % arms.len() as u64) as usize;
    arms[idx]
}

fn weighted_pick(experiment: &ExperimentConfig, user_id: UserId) -> &Variant {
    // Validation guarantees at least one variant with strictly positive
    // weight, so the cumulative total is finite and `> 0.0` and the
    // index lookup below cannot fall off the end.
    let total: f32 = experiment.variants.iter().map(|v| v.weight).sum();
    let seed = if experiment.sticky_by_user {
        sticky_seed(user_id, &experiment.name)
    } else {
        per_request_seed(user_id, &experiment.name)
    };
    let target = (seed as f64 / u64::MAX as f64) as f32 * total;
    let mut acc = 0.0;
    for variant in &experiment.variants {
        acc += variant.weight;
        if target < acc {
            return variant;
        }
    }
    experiment
        .variants
        .last()
        .expect("validation rejects experiments with no variants")
}

fn sticky_seed(user_id: UserId, experiment_name: &str) -> u64 {
    let mut hasher = Fnv64::new();
    hasher.write(user_id.0.as_bytes());
    hasher.write(experiment_name.as_bytes());
    hasher.finish()
}

fn per_request_seed(user_id: UserId, experiment_name: &str) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut hasher = Fnv64::new();
    hasher.write(user_id.0.as_bytes());
    hasher.write(experiment_name.as_bytes());
    hasher.write(&nanos.to_le_bytes());
    hasher.finish()
}

/// FNV-1a 64-bit. Tiny, zero-deps, and deterministic across builds — no
/// `DefaultHasher` (whose seed varies per process).
struct Fnv64 {
    state: u64,
}

impl Fnv64 {
    fn new() -> Self {
        Self {
            state: 0xcbf29ce484222325,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(self) -> u64 {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn experiment(sticky: bool, weights: &[(&str, f32)]) -> ExperimentConfig {
        ExperimentConfig {
            bandit_window_seconds: None,
            epsilon: None,
            metric: None,
            min_samples: None,
            name: "alice".to_string(),
            primary: None,
            purpose: None,
            sampling_rate: None,
            sticky_by_user: sticky,
            strategy: Strategy::Split,
            variants: weights
                .iter()
                .map(|(agent, weight)| Variant {
                    agent: (*agent).to_string(),
                    weight: *weight,
                })
                .collect(),
        }
    }

    fn shadow_experiment(primary: &str, variants: &[&str]) -> ExperimentConfig {
        ExperimentConfig {
            bandit_window_seconds: None,
            epsilon: None,
            metric: None,
            min_samples: None,
            name: "alice".to_string(),
            primary: Some(primary.to_string()),
            purpose: None,
            sampling_rate: None,
            sticky_by_user: true,
            strategy: Strategy::Shadow,
            variants: variants
                .iter()
                .map(|v| Variant {
                    agent: (*v).to_string(),
                    weight: 1.0,
                })
                .collect(),
        }
    }

    fn bandit_experiment(min_samples: u32, epsilon: f32, variants: &[&str]) -> ExperimentConfig {
        ExperimentConfig {
            bandit_window_seconds: None,
            epsilon: Some(epsilon),
            metric: Some("quality.helpfulness".to_string()),
            min_samples: Some(min_samples),
            name: "alice".to_string(),
            primary: None,
            purpose: None,
            sampling_rate: None,
            sticky_by_user: true,
            strategy: Strategy::Bandit,
            variants: variants
                .iter()
                .map(|v| Variant {
                    agent: (*v).to_string(),
                    weight: 1.0,
                })
                .collect(),
        }
    }

    #[test]
    fn unknown_name_is_passed_through() {
        let router = ExperimentRouter::new(vec![]);
        let resolved = router.resolve("solo", UserId::new());
        assert_eq!(resolved.agent.as_ref(), "solo");
        assert!(resolved.experiment.is_none());
    }

    #[test]
    fn experiment_resolves_to_a_variant() {
        let router = ExperimentRouter::new(vec![experiment(true, &[("v1", 1.0), ("v2", 1.0)])]);
        let resolved = router.resolve("alice", UserId::new());
        assert_eq!(resolved.experiment, Some("alice"));
        assert!(matches!(resolved.agent.as_ref(), "v1" | "v2"));
    }

    #[test]
    fn sticky_routing_is_stable_for_the_same_user() {
        let router = ExperimentRouter::new(vec![experiment(true, &[("v1", 1.0), ("v2", 1.0)])]);
        let user = UserId::new();
        let first = router.resolve("alice", user).agent.into_owned();
        for _ in 0..100 {
            assert_eq!(router.resolve("alice", user).agent.as_ref(), first);
        }
    }

    #[test]
    fn split_distribution_respects_weights_in_aggregate() {
        // Heavy skew so the test is robust to UUID hash bias on small N.
        let router = ExperimentRouter::new(vec![experiment(true, &[("v1", 9.0), ("v2", 1.0)])]);
        let mut v1 = 0;
        let mut v2 = 0;
        for _ in 0..2000 {
            match router.resolve("alice", UserId::new()).agent.as_ref() {
                "v1" => v1 += 1,
                "v2" => v2 += 1,
                _ => unreachable!(),
            }
        }
        // Expect roughly 90/10. Allow a generous band around the mean.
        assert!(v1 > v2 * 4, "expected v1 to dominate, got v1={v1} v2={v2}");
    }

    #[test]
    fn shadow_resolves_to_primary_for_every_user() {
        let exp = shadow_experiment("v1", &["v1", "v2", "v3"]);
        let router = ExperimentRouter::new(vec![exp]);
        for _ in 0..10 {
            assert_eq!(router.resolve("alice", UserId::new()).agent.as_ref(), "v1");
        }
    }

    #[test]
    fn shadow_variants_iterates_non_primary_in_order() {
        let exp = shadow_experiment("v1", &["v1", "v2", "v3"]);
        let router = ExperimentRouter::new(vec![exp.clone()]);
        let names: Vec<&str> = router
            .shadow_variants(router.get("alice").unwrap())
            .map(|v| v.agent.as_str())
            .collect();
        assert_eq!(names, vec!["v2", "v3"]);
    }

    #[test]
    fn shadow_should_sample_uses_sampling_rate() {
        let mut exp = shadow_experiment("v1", &["v1", "v2"]);
        exp.sampling_rate = Some(0.0);
        let router = ExperimentRouter::new(vec![exp]);
        let exp_ref = router.get("alice").unwrap();
        for _ in 0..10 {
            assert!(!router.shadow_should_sample(exp_ref, UserId::new()));
        }
    }

    #[test]
    fn bandit_forces_arms_below_min_samples() {
        let exp = bandit_experiment(30, 0.0, &["v1", "v2"]);
        let router = ExperimentRouter::new(vec![exp]);
        // Both arms have fewer than 30 samples — the picker must
        // choose one of them, not panic.
        let scores = vec![
            AgentScoreSummary {
                agent_name: "v1".into(),
                mean: 8.0,
                samples: 5,
            },
            AgentScoreSummary {
                agent_name: "v2".into(),
                mean: 1.0,
                samples: 5,
            },
        ];
        let resolved = router.resolve_with_scores("alice", UserId::new(), &scores);
        assert!(matches!(resolved.agent.as_ref(), "v1" | "v2"));
    }

    #[test]
    fn bandit_exploits_leader_when_above_min_samples() {
        // epsilon=0.0 so we never explore — pure exploitation.
        let exp = bandit_experiment(10, 0.0, &["v1", "v2"]);
        let router = ExperimentRouter::new(vec![exp]);
        let scores = vec![
            AgentScoreSummary {
                agent_name: "v1".into(),
                mean: 9.0,
                samples: 20,
            },
            AgentScoreSummary {
                agent_name: "v2".into(),
                mean: 4.0,
                samples: 20,
            },
        ];
        for _ in 0..10 {
            let resolved = router.resolve_with_scores("alice", UserId::new(), &scores);
            assert_eq!(resolved.agent.as_ref(), "v1");
        }
    }

    #[test]
    fn bandit_explores_with_high_epsilon() {
        // epsilon=1.0 — never exploit, always explore (uniform across arms).
        let exp = bandit_experiment(10, 1.0, &["v1", "v2"]);
        let router = ExperimentRouter::new(vec![exp]);
        let scores = vec![
            AgentScoreSummary {
                agent_name: "v1".into(),
                mean: 9.0,
                samples: 20,
            },
            AgentScoreSummary {
                agent_name: "v2".into(),
                mean: 1.0,
                samples: 20,
            },
        ];
        let mut v1 = 0;
        let mut v2 = 0;
        for _ in 0..2000 {
            match router
                .resolve_with_scores("alice", UserId::new(), &scores)
                .agent
                .as_ref()
            {
                "v1" => v1 += 1,
                "v2" => v2 += 1,
                _ => unreachable!(),
            }
        }
        // Uniform exploration over 2 arms — each should be ~50%. Use a
        // generous band to absorb hash skew on a small sample size.
        assert!(
            v1 > v2 / 3 && v2 > v1 / 3,
            "expected near-50/50 split, got v1={v1} v2={v2}"
        );
    }
}
