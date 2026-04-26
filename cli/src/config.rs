//! YAML root and cross-feature validation.
//!
//! Each feature crate owns its own `Config` slice; this file is purely
//! the composition. `Config` collects every slice into one struct
//! that mirrors the top-level YAML; `validate` runs the cross-feature
//! checks (agent → provider, agent → judge, experiment variant →
//! agent, etc.) that no single feature crate can do alone.

use std::collections::{HashMap, HashSet};
use std::{fs, path::Path};

use agents::AgentConfig;
use experiments::{ExperimentConfig, Strategy};
use judges::JudgeConfig;
use mcp::McpServerConfig;
use memory::MemoryConfig;
use providers::{ProviderConfig, ProviderKind};
use serde::Deserialize;
use studio::StudioConfig;
use telemetry::Config as TelemetryConfig;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub agents: Vec<AgentConfig>,
    /// Fallback user identifier for requests that don't carry a
    /// `safety_identifier` (or the deprecated `user` field). Unset means
    /// every request must supply its own identifier — appropriate for
    /// multi-tenant deployments. Set to e.g. `"main"` for single-user or
    /// local-dev setups so behavior stays identical whether or not the
    /// client bothers to send an id; the same memory bucket is used.
    #[serde(default)]
    pub default_user_id: Option<String>,
    /// A/B test groups that wrap one or more agents under a single
    /// addressable name. Clients send the experiment name as the `model`
    /// field; the router picks a variant per request (sticky-by-user by
    /// default). Experiment names share the agent namespace — collisions
    /// are rejected at config load.
    #[serde(default)]
    pub experiments: Vec<ExperimentConfig>,
    /// LLM-as-judge evaluators. Each agent opts in by listing judge names in
    /// its own `judges:` array — omit here (or on the agent) to skip
    /// evaluation entirely.
    #[serde(default)]
    pub judges: Vec<JudgeConfig>,
    #[serde(default)]
    pub mcp: HashMap<String, McpServerConfig>,
    /// Memory subsystem config (persistence, embedder, auto-extraction).
    /// Omit to use sensible defaults for local development.
    #[serde(default)]
    pub memory: MemoryConfig,
    pub providers: HashMap<ProviderKind, ProviderConfig>,
    /// Authentication for the studio UI and JSON API under `/studio`. Omit
    /// to leave the studio surface unauthenticated — fine for local dev,
    /// never for anything exposed beyond loopback.
    #[serde(default)]
    pub studio: Option<StudioConfig>,
    /// Observability wiring: stderr fmt logs (always on by default),
    /// SQLite mirror that drives the studio UI (on by default), and an
    /// optional OpenTelemetry OTLP exporter for shipping traces to
    /// Grafana / SigNoz / Jaeger / etc.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

impl Config {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| ConfigError::ReadConfig {
            path: path.display().to_string(),
            source,
        })?;
        let config: Self = serde_yaml::from_str(&contents).map_err(ConfigError::ParseConfig)?;
        config.validate()?;
        Ok(config)
    }

    /// Whole-graph schema validation. Run once on YAML load and again on
    /// every runtime mutation so cross-references (agent → provider, agent
    /// → judge, agent → mcp, agent → subagent) stay consistent.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.agents.is_empty() {
            return Err(ConfigError::NoAgents);
        }
        if let Some(id) = &self.default_user_id
            && id.trim().is_empty()
        {
            return Err(ConfigError::BlankDefaultUserId);
        }
        if let Some(studio) = &self.studio {
            match (&studio.basic, &studio.oidc) {
                (None, None) => return Err(ConfigError::StudioWithoutAuth),
                (Some(_), Some(_)) => return Err(ConfigError::StudioBothAuthMethods),
                (Some(basic), None) => {
                    if basic.password.is_empty() {
                        return Err(ConfigError::BlankStudioPassword);
                    }
                    if basic.username.is_empty() {
                        return Err(ConfigError::BlankStudioUsername);
                    }
                }
                (None, Some(oidc)) => {
                    if oidc.client_id.is_empty() {
                        return Err(ConfigError::BlankStudioOidcField("client_id"));
                    }
                    if oidc.issuer_url.is_empty() {
                        return Err(ConfigError::BlankStudioOidcField("issuer_url"));
                    }
                    if oidc.redirect_url.is_empty() {
                        return Err(ConfigError::BlankStudioOidcField("redirect_url"));
                    }
                }
            }
        }
        let mut judge_names = HashSet::new();
        for judge in &self.judges {
            if !judge_names.insert(&judge.name) {
                return Err(ConfigError::DuplicateJudge(judge.name.clone()));
            }
            if judge.rubrics.is_empty() {
                return Err(ConfigError::JudgeWithoutRubrics(judge.name.clone()));
            }
            if !(0.0..=1.0).contains(&judge.sampling_rate) {
                return Err(ConfigError::InvalidSamplingRate {
                    judge: judge.name.clone(),
                    value: judge.sampling_rate,
                });
            }
            let provider = ProviderKind::parse(&judge.provider).ok_or_else(|| {
                ConfigError::JudgeUnknownProvider {
                    judge: judge.name.clone(),
                    provider: judge.provider.clone(),
                }
            })?;
            if !self.providers.contains_key(&provider) {
                return Err(ConfigError::JudgeProviderNotConfigured {
                    judge: judge.name.clone(),
                    provider,
                });
            }
        }
        let mut seen = HashSet::new();
        for agent in &self.agents {
            if !seen.insert(&agent.name) {
                return Err(ConfigError::DuplicateAgent(agent.name.clone()));
            }
            if !self.providers.contains_key(&agent.provider) {
                return Err(ConfigError::ProviderNotConfigured {
                    agent: agent.name.clone(),
                    provider: agent.provider,
                });
            }
            for access in &agent.mcp_tools {
                if !self.mcp.contains_key(&access.server) {
                    return Err(ConfigError::McpServerNotConfigured {
                        agent: agent.name.clone(),
                        server: access.server.clone(),
                    });
                }
            }
            for judge_name in &agent.judges {
                if !judge_names.contains(judge_name) {
                    return Err(ConfigError::JudgeNotConfigured {
                        agent: agent.name.clone(),
                        judge: judge_name.clone(),
                    });
                }
            }
        }
        let agent_names: HashSet<&str> = self.agents.iter().map(|a| a.name.as_str()).collect();

        let mut experiment_names: HashSet<&str> = HashSet::new();
        for experiment in &self.experiments {
            if agent_names.contains(experiment.name.as_str()) {
                return Err(ConfigError::ExperimentAgentNameCollision(
                    experiment.name.clone(),
                ));
            }
            if !experiment_names.insert(experiment.name.as_str()) {
                return Err(ConfigError::ExperimentNameCollision(
                    experiment.name.clone(),
                ));
            }
            if experiment.variants.is_empty() {
                return Err(ConfigError::ExperimentWithoutVariants(
                    experiment.name.clone(),
                ));
            }
            let mut variant_seen = HashSet::new();
            for variant in &experiment.variants {
                if !agent_names.contains(variant.agent.as_str()) {
                    return Err(ConfigError::ExperimentUnknownVariant {
                        agent: variant.agent.clone(),
                        experiment: experiment.name.clone(),
                    });
                }
                if !variant_seen.insert(variant.agent.as_str()) {
                    return Err(ConfigError::ExperimentDuplicateVariant {
                        agent: variant.agent.clone(),
                        experiment: experiment.name.clone(),
                    });
                }
                if variant.weight <= 0.0 || !variant.weight.is_finite() {
                    return Err(ConfigError::ExperimentInvalidWeight {
                        agent: variant.agent.clone(),
                        experiment: experiment.name.clone(),
                        weight: variant.weight,
                    });
                }
            }
            validate_experiment_strategy_fields(self, experiment)?;
        }

        // Subagent references resolve against the *combined* namespace of
        // agents + experiments so an agent can list an experiment as a
        // subagent. Self-reference and duplicate detection still apply
        // exactly as before.
        for agent in &self.agents {
            let mut sub_seen = HashSet::new();
            for sub in &agent.subagents {
                if sub == &agent.name {
                    return Err(ConfigError::SelfSubagent(agent.name.clone()));
                }
                if !agent_names.contains(sub.as_str()) && !experiment_names.contains(sub.as_str()) {
                    return Err(ConfigError::UnknownSubagent {
                        agent: agent.name.clone(),
                        subagent: sub.clone(),
                    });
                }
                if !sub_seen.insert(sub) {
                    return Err(ConfigError::DuplicateSubagent {
                        agent: agent.name.clone(),
                        subagent: sub.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Strategy-specific field gating. Each strategy owns a small set of
/// optional fields; the others must be unset. Keeps mistakes (a `metric:`
/// hanging off a `split` experiment, say) loud at startup.
fn validate_experiment_strategy_fields(
    config: &Config,
    experiment: &ExperimentConfig,
) -> Result<(), ConfigError> {
    match experiment.strategy {
        Strategy::Split => {
            reject_field(
                experiment,
                "primary",
                experiment.primary.is_some(),
                "shadow",
            )?;
            reject_field(
                experiment,
                "sampling_rate",
                experiment.sampling_rate.is_some(),
                "shadow",
            )?;
            reject_field(experiment, "metric", experiment.metric.is_some(), "bandit")?;
            reject_field(
                experiment,
                "epsilon",
                experiment.epsilon.is_some(),
                "bandit",
            )?;
            reject_field(
                experiment,
                "min_samples",
                experiment.min_samples.is_some(),
                "bandit",
            )?;
            reject_field(
                experiment,
                "bandit_window_seconds",
                experiment.bandit_window_seconds.is_some(),
                "bandit",
            )?;
        }
        Strategy::Shadow => {
            reject_field(experiment, "metric", experiment.metric.is_some(), "bandit")?;
            reject_field(
                experiment,
                "epsilon",
                experiment.epsilon.is_some(),
                "bandit",
            )?;
            reject_field(
                experiment,
                "min_samples",
                experiment.min_samples.is_some(),
                "bandit",
            )?;
            reject_field(
                experiment,
                "bandit_window_seconds",
                experiment.bandit_window_seconds.is_some(),
                "bandit",
            )?;
            let Some(primary) = experiment.primary.as_deref() else {
                return Err(ConfigError::ShadowWithoutPrimary(experiment.name.clone()));
            };
            if !experiment.variants.iter().any(|v| v.agent == primary) {
                return Err(ConfigError::ExperimentPrimaryNotVariant {
                    experiment: experiment.name.clone(),
                    primary: primary.to_string(),
                });
            }
            if let Some(rate) = experiment.sampling_rate
                && !(0.0..=1.0).contains(&rate)
            {
                return Err(ConfigError::ExperimentInvalidSamplingRate {
                    experiment: experiment.name.clone(),
                    value: rate,
                });
            }
        }
        Strategy::Bandit => {
            reject_field(
                experiment,
                "primary",
                experiment.primary.is_some(),
                "shadow",
            )?;
            reject_field(
                experiment,
                "sampling_rate",
                experiment.sampling_rate.is_some(),
                "shadow",
            )?;
            let Some(metric) = experiment.metric.as_deref() else {
                return Err(ConfigError::BanditWithoutMetric(experiment.name.clone()));
            };
            let (judge_name, criterion) =
                metric
                    .split_once('.')
                    .ok_or_else(|| ConfigError::ExperimentMetricMalformed {
                        experiment: experiment.name.clone(),
                        metric: metric.to_string(),
                    })?;
            let judge = config
                .judges
                .iter()
                .find(|j| j.name == judge_name)
                .ok_or_else(|| ConfigError::ExperimentMetricUnknownJudge {
                    experiment: experiment.name.clone(),
                    judge: judge_name.to_string(),
                })?;
            if !judge.rubrics.contains_key(criterion) {
                return Err(ConfigError::ExperimentMetricUnknownCriterion {
                    criterion: criterion.to_string(),
                    experiment: experiment.name.clone(),
                    judge: judge_name.to_string(),
                });
            }
            for variant in &experiment.variants {
                let agent = config
                    .agents
                    .iter()
                    .find(|a| a.name == variant.agent)
                    .expect("variant agent existence is validated upstream");
                if !agent.judges.iter().any(|j| j == judge_name) {
                    return Err(ConfigError::ExperimentMetricVariantMissingJudge {
                        agent: variant.agent.clone(),
                        experiment: experiment.name.clone(),
                        judge: judge_name.to_string(),
                        metric: metric.to_string(),
                    });
                }
            }
            if let Some(epsilon) = experiment.epsilon
                && !(0.0..=1.0).contains(&epsilon)
            {
                return Err(ConfigError::ExperimentInvalidEpsilon {
                    experiment: experiment.name.clone(),
                    value: epsilon,
                });
            }
        }
    }
    Ok(())
}

fn reject_field(
    experiment: &ExperimentConfig,
    field: &'static str,
    present: bool,
    valid_for: &'static str,
) -> Result<(), ConfigError> {
    if present {
        return Err(ConfigError::ExperimentFieldStrategyMismatch {
            experiment: experiment.name.clone(),
            field,
            strategy: match experiment.strategy {
                Strategy::Bandit => "bandit",
                Strategy::Shadow => "shadow",
                Strategy::Split => "split",
            },
            valid_for,
        });
    }
    Ok(())
}

/// Errors raised while loading and validating `coulisse.yaml`. Pure
/// schema/coverage checks — anything that needs to talk to a running
/// process (MCP servers, providers) belongs in the agents crate or its
/// downstream errors instead.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("default_user_id must be non-empty when set")]
    BlankDefaultUserId,
    #[error("studio.oidc.{0} must be non-empty")]
    BlankStudioOidcField(&'static str),
    #[error("studio.basic.password must be non-empty")]
    BlankStudioPassword,
    #[error("studio.basic.username must be non-empty")]
    BlankStudioUsername,
    #[error("duplicate agent name in config: {0}")]
    DuplicateAgent(String),
    #[error("duplicate judge name in config: {0}")]
    DuplicateJudge(String),
    #[error("agent '{agent}' lists subagent '{subagent}' more than once")]
    DuplicateSubagent { agent: String, subagent: String },
    #[error(
        "experiment '{0}' shares a name with an agent; rename one — experiment and agent names share a single namespace"
    )]
    ExperimentAgentNameCollision(String),
    #[error("experiment '{experiment}' lists variant agent '{agent}' more than once")]
    ExperimentDuplicateVariant { agent: String, experiment: String },
    #[error("experiment '{experiment}' has variant '{agent}' with non-positive weight {weight}")]
    ExperimentInvalidWeight {
        agent: String,
        experiment: String,
        weight: f32,
    },
    #[error("duplicate experiment name in config: {0}")]
    ExperimentNameCollision(String),
    #[error(
        "experiment '{experiment}' references variant agent '{agent}' which is not defined under `agents:`"
    )]
    ExperimentUnknownVariant { agent: String, experiment: String },
    #[error("experiment '{0}' must declare at least one variant")]
    ExperimentWithoutVariants(String),
    #[error(
        "experiment '{experiment}' uses strategy '{strategy}' but sets '{field}', which is only valid for {valid_for}"
    )]
    ExperimentFieldStrategyMismatch {
        experiment: String,
        field: &'static str,
        strategy: &'static str,
        valid_for: &'static str,
    },
    #[error("experiment '{experiment}' has epsilon={value}, must be in [0.0, 1.0]")]
    ExperimentInvalidEpsilon { experiment: String, value: f32 },
    #[error("experiment '{experiment}' has sampling_rate={value}, must be in [0.0, 1.0]")]
    ExperimentInvalidSamplingRate { experiment: String, value: f32 },
    #[error("experiment '{experiment}' metric '{metric}' must look like 'judge.criterion'")]
    ExperimentMetricMalformed { experiment: String, metric: String },
    #[error(
        "experiment '{experiment}' metric references unknown criterion '{criterion}' on judge '{judge}'"
    )]
    ExperimentMetricUnknownCriterion {
        criterion: String,
        experiment: String,
        judge: String,
    },
    #[error("experiment '{experiment}' metric references unknown judge '{judge}'")]
    ExperimentMetricUnknownJudge { experiment: String, judge: String },
    #[error(
        "experiment '{experiment}' uses bandit metric '{metric}' but variant '{agent}' does not opt into judge '{judge}'"
    )]
    ExperimentMetricVariantMissingJudge {
        agent: String,
        experiment: String,
        judge: String,
        metric: String,
    },
    #[error("experiment '{experiment}' has primary '{primary}' which is not one of its variants")]
    ExperimentPrimaryNotVariant { experiment: String, primary: String },
    #[error("experiment '{0}' uses strategy 'shadow' but does not declare a primary variant")]
    ShadowWithoutPrimary(String),
    #[error("experiment '{0}' uses strategy 'bandit' but does not declare a metric")]
    BanditWithoutMetric(String),
    #[error("judge '{judge}' has sampling_rate={value}, must be in [0.0, 1.0]")]
    InvalidSamplingRate { judge: String, value: f32 },
    #[error("agent '{agent}' references judge '{judge}' which is not configured")]
    JudgeNotConfigured { agent: String, judge: String },
    #[error(
        "judge '{judge}' references provider '{provider}' which is not declared under `providers:`"
    )]
    JudgeProviderNotConfigured {
        judge: String,
        provider: ProviderKind,
    },
    #[error(
        "judge '{judge}' provider '{provider}' is not supported (anthropic, cohere, deepseek, gemini, groq, openai)"
    )]
    JudgeUnknownProvider { judge: String, provider: String },
    #[error("judge '{0}' declares no rubrics; add at least one `criterion: description` entry")]
    JudgeWithoutRubrics(String),
    #[error("agent '{agent}' references MCP server '{server}' which is not configured")]
    McpServerNotConfigured { agent: String, server: String },
    #[error("config must declare at least one agent")]
    NoAgents,
    #[error("failed to parse config: {0}")]
    ParseConfig(serde_yaml::Error),
    #[error("agent '{agent}' references provider '{provider}' which is not configured")]
    ProviderNotConfigured {
        agent: String,
        provider: ProviderKind,
    },
    #[error("failed to read config file {path}: {source}")]
    ReadConfig {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("agent '{0}' cannot list itself as a subagent")]
    SelfSubagent(String),
    #[error("studio block must declare exactly one of `basic` or `oidc`, not both (remove one)")]
    StudioBothAuthMethods,
    #[error(
        "studio block must declare one of `basic` or `oidc` (or remove the block to disable auth)"
    )]
    StudioWithoutAuth,
    #[error("agent '{agent}' references subagent '{subagent}' which is not defined")]
    UnknownSubagent { agent: String, subagent: String },
}
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<Config, ConfigError> {
        let config: Config = serde_yaml::from_str(yaml).map_err(ConfigError::ParseConfig)?;
        config.validate()?;
        Ok(config)
    }

    const BASE_PROVIDERS: &str = r#"
providers:
  openai:
    api_key: test
"#;

    #[test]
    fn subagents_and_purpose_parse_and_validate() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: coach
    provider: openai
    model: gpt-4
    subagents: [onboarder]
  - name: onboarder
    provider: openai
    model: gpt-4
    purpose: Gather profile fields.
"
        );
        let config = parse(&yaml).expect("valid config");
        let coach = config.agents.iter().find(|a| a.name == "coach").unwrap();
        assert_eq!(coach.subagents, vec!["onboarder".to_string()]);
        let onboarder = config
            .agents
            .iter()
            .find(|a| a.name == "onboarder")
            .unwrap();
        assert_eq!(onboarder.purpose.as_deref(), Some("Gather profile fields."));
    }

    #[test]
    fn agents_without_subagents_or_purpose_still_parse() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: solo
    provider: openai
    model: gpt-4
"
        );
        let config = parse(&yaml).expect("minimal agent config");
        assert_eq!(config.agents[0].subagents.len(), 0);
        assert!(config.agents[0].purpose.is_none());
    }

    #[test]
    fn self_subagent_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: loopy
    provider: openai
    model: gpt-4
    subagents: [loopy]
"
        );
        match parse(&yaml) {
            Err(ConfigError::SelfSubagent(name)) => assert_eq!(name, "loopy"),
            other => panic!("expected SelfSubagent error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_subagent_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: coach
    provider: openai
    model: gpt-4
    subagents: [ghost]
"
        );
        match parse(&yaml) {
            Err(ConfigError::UnknownSubagent { agent, subagent }) => {
                assert_eq!(agent, "coach");
                assert_eq!(subagent, "ghost");
            }
            other => panic!("expected UnknownSubagent error, got {other:?}"),
        }
    }

    #[test]
    fn experiment_with_split_strategy_parses() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
  - name: alice-v2
    provider: openai
    model: gpt-4o
experiments:
  - name: alice
    strategy: split
    variants:
      - agent: alice-v1
        weight: 0.7
      - agent: alice-v2
        weight: 0.3
"
        );
        let config = parse(&yaml).expect("valid experiment");
        assert_eq!(config.experiments.len(), 1);
        let exp = &config.experiments[0];
        assert_eq!(exp.name, "alice");
        assert!(exp.sticky_by_user);
        assert_eq!(exp.variants.len(), 2);
    }

    #[test]
    fn experiment_can_be_listed_as_subagent() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
  - name: alice-v2
    provider: openai
    model: gpt-4
  - name: orchestrator
    provider: openai
    model: gpt-4
    subagents: [alice]
experiments:
  - name: alice
    strategy: split
    variants:
      - agent: alice-v1
      - agent: alice-v2
"
        );
        parse(&yaml).expect("experiment as subagent should validate");
    }

    #[test]
    fn experiment_name_colliding_with_agent_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice
    provider: openai
    model: gpt-4
  - name: alice-v2
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: split
    variants:
      - agent: alice
      - agent: alice-v2
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentAgentNameCollision(name)) => assert_eq!(name, "alice"),
            other => panic!("expected ExperimentAgentNameCollision, got {other:?}"),
        }
    }

    #[test]
    fn experiment_with_unknown_variant_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: split
    variants:
      - agent: alice-v1
      - agent: ghost
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentUnknownVariant { agent, experiment }) => {
                assert_eq!(agent, "ghost");
                assert_eq!(experiment, "alice");
            }
            other => panic!("expected ExperimentUnknownVariant, got {other:?}"),
        }
    }

    #[test]
    fn experiment_with_zero_weight_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: split
    variants:
      - agent: alice-v1
        weight: 0
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentInvalidWeight {
                agent, experiment, ..
            }) => {
                assert_eq!(agent, "alice-v1");
                assert_eq!(experiment, "alice");
            }
            other => panic!("expected ExperimentInvalidWeight, got {other:?}"),
        }
    }

    #[test]
    fn experiment_with_no_variants_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: split
    variants: []
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentWithoutVariants(name)) => assert_eq!(name, "alice"),
            other => panic!("expected ExperimentWithoutVariants, got {other:?}"),
        }
    }

    #[test]
    fn shadow_experiment_parses_with_primary_and_sampling_rate() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
  - name: alice-v2
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: shadow
    primary: alice-v1
    sampling_rate: 0.25
    variants:
      - agent: alice-v1
      - agent: alice-v2
"
        );
        let config = parse(&yaml).expect("valid shadow experiment");
        assert!(matches!(config.experiments[0].strategy, Strategy::Shadow));
        assert_eq!(config.experiments[0].primary.as_deref(), Some("alice-v1"));
        assert_eq!(config.experiments[0].sampling_rate, Some(0.25));
    }

    #[test]
    fn shadow_without_primary_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: shadow
    variants:
      - agent: alice-v1
"
        );
        match parse(&yaml) {
            Err(ConfigError::ShadowWithoutPrimary(name)) => assert_eq!(name, "alice"),
            other => panic!("expected ShadowWithoutPrimary, got {other:?}"),
        }
    }

    #[test]
    fn shadow_primary_must_be_a_variant() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
  - name: alice-v2
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: shadow
    primary: alice-v2
    variants:
      - agent: alice-v1
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentPrimaryNotVariant { primary, .. }) => {
                assert_eq!(primary, "alice-v2")
            }
            other => panic!("expected ExperimentPrimaryNotVariant, got {other:?}"),
        }
    }

    #[test]
    fn split_with_primary_field_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: split
    primary: alice-v1
    variants:
      - agent: alice-v1
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentFieldStrategyMismatch { field, .. }) => {
                assert_eq!(field, "primary")
            }
            other => panic!("expected ExperimentFieldStrategyMismatch, got {other:?}"),
        }
    }

    #[test]
    fn bandit_experiment_parses_with_metric() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
    judges: [quality]
  - name: alice-v2
    provider: openai
    model: gpt-4
    judges: [quality]
judges:
  - name: quality
    provider: openai
    model: gpt-4
    rubrics:
      helpfulness: Whether the assistant answered.
experiments:
  - name: alice
    strategy: bandit
    metric: quality.helpfulness
    epsilon: 0.2
    min_samples: 10
    bandit_window_seconds: 86400
    variants:
      - agent: alice-v1
      - agent: alice-v2
"
        );
        let config = parse(&yaml).expect("valid bandit experiment");
        assert!(matches!(config.experiments[0].strategy, Strategy::Bandit));
        assert_eq!(
            config.experiments[0].metric.as_deref(),
            Some("quality.helpfulness")
        );
        assert_eq!(config.experiments[0].epsilon, Some(0.2));
    }

    #[test]
    fn bandit_metric_must_reference_known_judge() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
    judges: [quality]
judges:
  - name: quality
    provider: openai
    model: gpt-4
    rubrics:
      helpfulness: Whether the assistant answered.
experiments:
  - name: alice
    strategy: bandit
    metric: ghost.helpfulness
    variants:
      - agent: alice-v1
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentMetricUnknownJudge { judge, .. }) => {
                assert_eq!(judge, "ghost")
            }
            other => panic!("expected ExperimentMetricUnknownJudge, got {other:?}"),
        }
    }

    #[test]
    fn bandit_metric_must_reference_known_criterion() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
    judges: [quality]
judges:
  - name: quality
    provider: openai
    model: gpt-4
    rubrics:
      helpfulness: Whether the assistant answered.
experiments:
  - name: alice
    strategy: bandit
    metric: quality.tone
    variants:
      - agent: alice-v1
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentMetricUnknownCriterion { criterion, .. }) => {
                assert_eq!(criterion, "tone")
            }
            other => panic!("expected ExperimentMetricUnknownCriterion, got {other:?}"),
        }
    }

    #[test]
    fn bandit_variant_must_opt_into_metric_judge() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
    judges: [quality]
  - name: alice-v2
    provider: openai
    model: gpt-4
judges:
  - name: quality
    provider: openai
    model: gpt-4
    rubrics:
      helpfulness: Whether the assistant answered.
experiments:
  - name: alice
    strategy: bandit
    metric: quality.helpfulness
    variants:
      - agent: alice-v1
      - agent: alice-v2
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentMetricVariantMissingJudge { agent, .. }) => {
                assert_eq!(agent, "alice-v2")
            }
            other => panic!("expected ExperimentMetricVariantMissingJudge, got {other:?}"),
        }
    }

    #[test]
    fn bandit_without_metric_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: bandit
    variants:
      - agent: alice-v1
"
        );
        match parse(&yaml) {
            Err(ConfigError::BanditWithoutMetric(name)) => assert_eq!(name, "alice"),
            other => panic!("expected BanditWithoutMetric, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_experiment_names_are_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: alice-v1
    provider: openai
    model: gpt-4
experiments:
  - name: alice
    strategy: split
    variants:
      - agent: alice-v1
  - name: alice
    strategy: split
    variants:
      - agent: alice-v1
"
        );
        match parse(&yaml) {
            Err(ConfigError::ExperimentNameCollision(name)) => assert_eq!(name, "alice"),
            other => panic!("expected ExperimentNameCollision, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_subagent_is_rejected() {
        let yaml = format!(
            "{BASE_PROVIDERS}agents:
  - name: coach
    provider: openai
    model: gpt-4
    subagents: [helper, helper]
  - name: helper
    provider: openai
    model: gpt-4
"
        );
        match parse(&yaml) {
            Err(ConfigError::DuplicateSubagent { agent, subagent }) => {
                assert_eq!(agent, "coach");
                assert_eq!(subagent, "helper");
            }
            other => panic!("expected DuplicateSubagent error, got {other:?}"),
        }
    }
}
