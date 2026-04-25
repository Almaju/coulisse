use thiserror::Error;

use crate::ProviderKind;

/// Errors raised while loading and validating `coulisse.yaml`. Pure
/// schema/coverage checks — anything that needs to talk to a running
/// process (MCP servers, providers) belongs in the prompter or its
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
