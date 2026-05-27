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
use auth::Config as AuthConfig;
use experiments::{ExperimentConfig, Strategy};
use judges::JudgeConfig;
use mcp::McpServerConfig;
use memory::MemoryYaml;
use providers::{ProviderConfig, ProviderKind};
use serde::Deserialize;
use sidecars::SidecarConfig;
use smoke::SmokeTestConfig;
use storage::StorageYaml;
use telemetry::Config as TelemetryConfig;
use thiserror::Error;
use triggers::TriggerConfig;

/// One document source for the RAG knowledge index.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeSource {
    /// Optional human-readable name. If omitted, derived from `source` path.
    /// Normalized to `[a-z0-9_]`; the slug itself must be ≤ 57 chars so
    /// the full tool name (`search_<slug>`, 7-char prefix) fits within 64.
    #[serde(default)]
    pub name: Option<String>,
    /// Local directory or file path to index.
    pub source: String,
    /// Chunking strategy: `chunk`, `page`, or `line`. Defaults to `chunk`.
    #[serde(default)]
    pub strategy: ChunkStrategy,
}

/// How source files are split into chunks for embedding.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChunkStrategy {
    #[default]
    Chunk,
    Line,
    Page,
}

/// Which embedding provider powers the knowledge index.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingsProvider {
    #[default]
    Local,
    Ollama,
    Openai,
}

/// Embedding configuration for the knowledge index.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct EmbeddingsConfig {
    /// Embedding model name. Ignored when `provider: local`.
    #[serde(default)]
    pub model: Option<String>,
    /// Embedding provider. Defaults to `local` (BGE-small-EN-v1.5 via fastembed).
    #[serde(default)]
    pub provider: EmbeddingsProvider,
}

/// Internal error for slug normalization.
#[derive(Debug)]
enum SlugError {
    /// Name contains non-ASCII characters; user must use ASCII instead.
    NonAscii,
}

/// Normalize a raw knowledge source name into a `[a-z0-9_]` slug.
///
/// Rules (in order):
/// 1. Reject if any character is non-ASCII — no transliteration magic.
/// 2. Lowercase the whole string.
/// 3. Replace hyphens, slashes, and any other non-`[a-z0-9]` characters
///    with underscores.
/// 4. Collapse consecutive underscores into one.
/// 5. Trim leading and trailing underscores.
///
/// Returns `SlugError::NonAscii` if the input contains non-ASCII bytes.
/// An empty result after trimming is valid here; the caller rejects it.
pub(crate) fn to_slug(name: &str) -> Result<String, SlugError> {
    if !name.is_ascii() {
        return Err(SlugError::NonAscii);
    }
    let lowered = name.to_ascii_lowercase();
    let replaced: String = lowered
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    // Collapse runs of underscores.
    let mut collapsed = String::with_capacity(replaced.len());
    let mut prev_underscore = false;
    for c in replaced.chars() {
        if c == '_' {
            if !prev_underscore {
                collapsed.push('_');
            }
            prev_underscore = true;
        } else {
            collapsed.push(c);
            prev_underscore = false;
        }
    }
    let trimmed = collapsed.trim_matches('_').to_string();
    Ok(trimmed)
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct Config {
    pub agents: Vec<AgentConfig>,
    /// Authentication for the OpenAI-compatible `/v1/*` proxy and the
    /// `/admin/*` (studio) surfaces. Each scope is independent: omit a
    /// scope to leave it unauthenticated (fine for local dev, never for
    /// anything exposed beyond loopback).
    #[serde(default)]
    pub auth: AuthConfig,
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
    /// Embedding configuration for the knowledge index. Defaults to local
    /// provider (BGE-small-EN-v1.5 via fastembed, ~130 MB, downloaded once).
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,
    /// RAG knowledge sources. Each entry is indexed at startup and exposes a
    /// `search_<name>` tool. Names are normalized to `[a-z0-9_]`; collisions
    /// after normalization are rejected before boot.
    #[serde(default)]
    pub knowledge: Vec<KnowledgeSource>,
    /// Memory subsystem config. Two pillars: `storage` (where data lives)
    /// and `user_state` (long-term per-user memory; off by default).
    /// Omit the whole block for sensible defaults — history-only on a
    /// local `SQLite` file.
    #[serde(default)]
    pub memory: MemoryYaml,
    /// HTTP port the proxy/admin server binds to. Defaults to 8421. Useful
    /// when running multiple Coulisse instances against different
    /// `coulisse.yaml` files on the same machine.
    #[serde(default)]
    pub port: Option<u16>,
    pub providers: HashMap<ProviderKind, ProviderConfig>,
    /// Long-lived helper processes Coulisse spawns alongside itself
    /// (chat-platform bridges, monitoring agents, anything you'd otherwise
    /// launch in a separate terminal). Each entry declares a command,
    /// optional args/env/cwd, and a restart policy. Coulisse captures
    /// stdout/stderr into its own log; non-zero exits restart per policy.
    #[serde(default)]
    pub sidecars: Vec<SidecarConfig>,
    /// Synthetic-user evaluation tests. Each entry pairs a persona prompt
    /// with a target agent (or experiment); admin UI exposes a "Run now"
    /// button that drives the conversation, persists every turn, and
    /// fans the assistant turns out to the configured judges. Useful for
    /// iterating on agent prompts and comparing experiment variants.
    #[serde(default)]
    pub smoke_tests: Vec<SmokeTestConfig>,
    /// File storage for multimodal uploads. Implements the OpenAI Files API
    /// (`POST /v1/files`, `GET /v1/files`, `DELETE /v1/files/:id`, etc.).
    /// Omit to disable file uploads entirely.
    #[serde(default)]
    pub storage: StorageYaml,
    /// Observability wiring: stderr fmt logs (always on by default),
    /// `SQLite` mirror that drives the studio UI (on by default), and an
    /// optional OpenTelemetry OTLP exporter for shipping traces to
    /// Grafana / `SigNoz` / Jaeger / etc.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// Time-based and event-based triggers that drop tasks on the
    /// background queue without anyone making an HTTP request. Cron is
    /// supported today; webhooks arrive in a follow-up. Each trigger
    /// names a target `agent` and a `prompt`; firing enqueues a task
    /// that workers run through the same handler as the sync chat
    /// endpoint.
    #[serde(default)]
    pub triggers: Vec<TriggerConfig>,
    /// Named text snippets you can splice into any string field using
    /// `${vars.<name>}`. Resolved after env-var expansion, single-pass
    /// (a var's value is not itself re-expanded). Useful for sharing
    /// preamble footers across agents, common paths, repeated strings.
    #[serde(default)]
    pub vars: HashMap<String, String>,
}

/// Tiny pre-parse target used to extract the `vars:` table before the
/// full `Config` deserialization. Lets us substitute `${vars.x}` in the
/// raw YAML *before* fields like `preamble` get parsed, so the
/// substitution is field-agnostic and works anywhere a string lives.
#[derive(Debug, Deserialize)]
struct VarsOnly {
    #[serde(default)]
    vars: HashMap<String, String>,
}

fn expand_env_vars(s: &str) -> Result<String, ExpandError> {
    expand_env_vars_with(s, |var| std::env::var(var).ok())
}

fn expand_env_vars_with(
    s: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String, ExpandError> {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let offset = (s.len() - rest.len()) + start;
        result.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest
            .find('}')
            .ok_or(ExpandError::UnclosedEnvVar { offset })?;
        let var = &rest[..end];
        // `${vars.*}` is resolved by `expand_config_vars` in a second
        // pass once the `vars:` table has been parsed — leave it intact.
        if var.starts_with("vars.") {
            result.push_str("${");
            result.push_str(var);
            result.push('}');
            rest = &rest[end + 1..];
            continue;
        }
        let value = lookup(var).ok_or_else(|| ExpandError::EnvVarNotSet {
            offset,
            var: var.to_string(),
        })?;
        result.push_str(&value);
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

/// Resolve `${vars.<name>}` placeholders against the `vars:` table.
/// Runs after env-var expansion, so any `${VAR}` is already gone.
/// Single-pass: a substituted value containing `${vars.x}` is **not**
/// itself re-expanded — keeps the loader simple and prevents cycles.
///
/// Multi-line values inherit the placeholder's leading indent so they
/// don't break YAML block scalars. The first line lands wherever the
/// `${...}` was in the source; every subsequent line gets prefixed with
/// the same leading whitespace as the placeholder's line. Without this,
/// splicing a multi-line snippet into `preamble: |` would collapse the
/// block's indentation contract and fail to parse.
fn expand_config_vars(s: &str, vars: &HashMap<String, String>) -> Result<String, ExpandError> {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${vars.") {
        let offset = (s.len() - rest.len()) + start;
        result.push_str(&rest[..start]);
        // Capture indent of the placeholder's line from what we've
        // written so far, *before* advancing `rest`.
        let indent = leading_indent_of_last_line(&result);
        rest = &rest[start + "${vars.".len()..];
        let end = rest
            .find('}')
            .ok_or(ExpandError::UnclosedEnvVar { offset })?;
        let name = &rest[..end];
        let value = vars.get(name).ok_or_else(|| ExpandError::ConfigVarNotSet {
            offset,
            var: name.to_string(),
        })?;
        let mut lines = value.split('\n');
        if let Some(first) = lines.next() {
            result.push_str(first);
        }
        for line in lines {
            result.push('\n');
            if !line.is_empty() {
                result.push_str(&indent);
            }
            result.push_str(line);
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

/// Leading whitespace of the last line of `s` — everything from the most
/// recent `\n` (or start of string) up to the first non-whitespace
/// character. Empty when the last line begins with non-whitespace.
fn leading_indent_of_last_line(s: &str) -> String {
    let line_start = s.rfind('\n').map_or(0, |i| i + 1);
    s[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Convert a byte offset into the source into (1-indexed line number,
/// full line content). Used to render env-var expansion errors with
/// location context.
fn locate(source: &str, offset: usize) -> (usize, String) {
    let clamped = offset.min(source.len());
    let mut line_number = 1;
    let mut line_start = 0;
    for (i, b) in source.as_bytes().iter().enumerate() {
        if i >= clamped {
            break;
        }
        if *b == b'\n' {
            line_number += 1;
            line_start = i + 1;
        }
    }
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |n| line_start + n);
    let line_content = source[line_start..line_end].to_string();
    (line_number, line_content)
}

impl Config {
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::ReadConfig {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_str(&raw, &path.display().to_string())
    }

    /// Load a config from a YAML string, using `path` only for error
    /// messages. Same pipeline as `from_path`: env-var expansion →
    /// `vars:` extraction → `${vars.x}` substitution → full parse →
    /// validation.
    ///
    /// # Errors
    ///
    /// Returns an error if expansion, parsing, or validation fails.
    pub fn from_str(raw: &str, path: &str) -> Result<Self, ConfigError> {
        let env_expanded = expand_env_vars(raw).map_err(|e| {
            let (line_number, line_content) = locate(raw, e.offset());
            match e {
                ExpandError::EnvVarNotSet { var, .. } => ConfigError::EnvVarNotSet {
                    line_content,
                    line_number,
                    path: path.to_string(),
                    var,
                },
                ExpandError::UnclosedEnvVar { .. } => ConfigError::UnclosedEnvVar {
                    line_content,
                    line_number,
                    path: path.to_string(),
                },
                ExpandError::ConfigVarNotSet { .. } => {
                    unreachable!("env-var pass cannot emit config-var errors")
                }
            }
        })?;
        let vars_only: VarsOnly =
            serde_yaml::from_str(&env_expanded).map_err(ConfigError::ParseConfig)?;
        let contents = expand_config_vars(&env_expanded, &vars_only.vars).map_err(|e| {
            // Locate against the env-expanded text — that's where the
            // offset points. Multi-line env-var values may shift line
            // numbers; the line content still shows the placeholder.
            let (line_number, line_content) = locate(&env_expanded, e.offset());
            match e {
                ExpandError::ConfigVarNotSet { var, .. } => ConfigError::ConfigVarNotSet {
                    line_content,
                    line_number,
                    path: path.to_string(),
                    var,
                },
                ExpandError::UnclosedEnvVar { .. } => ConfigError::UnclosedEnvVar {
                    line_content,
                    line_number,
                    path: path.to_string(),
                },
                ExpandError::EnvVarNotSet { .. } => {
                    unreachable!("config-var pass cannot emit env-var errors")
                }
            }
        })?;
        let config: Self = serde_yaml::from_str(&contents).map_err(ConfigError::ParseConfig)?;
        config.validate()?;
        Ok(config)
    }

    /// Whole-graph schema validation. Run once on YAML load and again on
    /// every runtime mutation so cross-references (agent → provider, agent
    /// → judge, agent → mcp, agent → subagent) stay consistent.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.agents.is_empty() {
            return Err(ConfigError::NoAgents);
        }
        if let Some(id) = &self.default_user_id
            && id.trim().is_empty()
        {
            return Err(ConfigError::BlankDefaultUserId);
        }
        self.auth.validate().map_err(ConfigError::Auth)?;
        self.validate_mcp_oauth()?;
        self.validate_knowledge()?;
        let judge_names = self.validate_judges()?;
        let agent_names = self.validate_agents(&judge_names)?;
        let experiment_names = self.validate_experiments(&agent_names)?;
        self.validate_smoke_tests(&agent_names, &experiment_names)?;
        self.validate_subagents(&agent_names, &experiment_names)?;
        self.validate_triggers(&agent_names, &experiment_names)?;
        self.validate_sidecars()?;
        Ok(())
    }

    fn validate_knowledge(&self) -> Result<(), ConfigError> {
        // Maps normalized slug → original name for collision detection.
        let mut seen: HashMap<String, String> = HashMap::new();
        for source in &self.knowledge {
            let raw = source.name.as_deref().unwrap_or(&source.source);
            let slug = to_slug(raw).map_err(|_| ConfigError::KnowledgeNonAsciiName {
                name: raw.to_string(),
            })?;
            if slug.is_empty() {
                return Err(ConfigError::KnowledgeEmptySlug {
                    name: raw.to_string(),
                });
            }
            // Tool will be exposed as `search_<slug>` (7-char prefix).
            if slug.len() + 7 > 64 {
                return Err(ConfigError::KnowledgeSlugTooLong {
                    name: raw.to_string(),
                    slug,
                });
            }
            if let Some(prev) = seen.get(&slug) {
                return Err(ConfigError::KnowledgeSlugCollision {
                    name_a: prev.clone(),
                    name_b: raw.to_string(),
                    slug,
                });
            }
            seen.insert(slug, raw.to_string());
        }
        Ok(())
    }

    fn validate_sidecars(&self) -> Result<(), ConfigError> {
        let mut seen: HashSet<&str> = HashSet::new();
        for s in &self.sidecars {
            if s.command.trim().is_empty() {
                return Err(ConfigError::SidecarBlankCommand(s.name.clone()));
            }
            if !seen.insert(s.name.as_str()) {
                return Err(ConfigError::DuplicateSidecar(s.name.clone()));
            }
        }
        Ok(())
    }

    fn validate_triggers(
        &self,
        agent_names: &HashSet<&str>,
        experiment_names: &HashSet<&str>,
    ) -> Result<(), ConfigError> {
        let mut seen_names: HashSet<&str> = HashSet::new();
        let mut seen_paths: HashSet<&str> = HashSet::new();
        for t in &self.triggers {
            if !seen_names.insert(t.name.as_str()) {
                return Err(ConfigError::DuplicateTrigger(t.name.clone()));
            }
            // Templated `agent:` fields (e.g. `agent: "{{agent}}"` for
            // webhooks) cannot be cross-validated at load time — the
            // value isn't known until a request arrives. Skip the check
            // and let the worker surface unknown-agent errors at run
            // time via the task's error state.
            let is_templated = t.agent.contains("{{");
            if !is_templated
                && !agent_names.contains(t.agent.as_str())
                && !experiment_names.contains(t.agent.as_str())
            {
                return Err(ConfigError::TriggerUnknownAgent {
                    agent: t.agent.clone(),
                    trigger: t.name.clone(),
                });
            }
            if let triggers::TriggerKind::Webhook { path } = &t.kind {
                if !path.starts_with("/hooks/") {
                    return Err(ConfigError::TriggerWebhookPathInvalid {
                        path: path.clone(),
                        trigger: t.name.clone(),
                    });
                }
                if !seen_paths.insert(path.as_str()) {
                    return Err(ConfigError::TriggerWebhookPathDuplicate {
                        path: path.clone(),
                        trigger: t.name.clone(),
                    });
                }
            }
        }
        triggers::validate_all(&self.triggers).map_err(ConfigError::Trigger)?;
        Ok(())
    }

    fn validate_mcp_oauth(&self) -> Result<(), ConfigError> {
        let has_oauth = self.mcp.values().any(|c| c.oauth.is_some());
        if !has_oauth {
            return Ok(());
        }
        if self.auth.mcp_consumer_secret.is_none() {
            return Err(ConfigError::McpOAuthMissingConsumerSecret);
        }
        if std::env::var("COULISSE_VAULT_KEY").is_err() {
            return Err(ConfigError::McpOAuthMissingVaultKey);
        }
        if std::env::var("COULISSE_HMAC_KEY").is_err() {
            return Err(ConfigError::McpOAuthMissingHmacKey);
        }
        for (name, cfg) in &self.mcp {
            let Some(oauth) = &cfg.oauth else {
                continue;
            };
            if oauth.authorization_url.is_empty() {
                return Err(ConfigError::McpOAuthBlankField {
                    field: "authorization_url",
                    server: name.clone(),
                });
            }
            if oauth.client_id.is_empty() {
                return Err(ConfigError::McpOAuthBlankField {
                    field: "client_id",
                    server: name.clone(),
                });
            }
            if oauth.client_secret.is_empty() {
                return Err(ConfigError::McpOAuthBlankField {
                    field: "client_secret",
                    server: name.clone(),
                });
            }
            if oauth.redirect_uri.is_empty() {
                return Err(ConfigError::McpOAuthBlankField {
                    field: "redirect_uri",
                    server: name.clone(),
                });
            }
            if oauth.token_url.is_empty() {
                return Err(ConfigError::McpOAuthBlankField {
                    field: "token_url",
                    server: name.clone(),
                });
            }
        }
        Ok(())
    }

    fn validate_agents(&self, judge_names: &HashSet<&str>) -> Result<HashSet<&str>, ConfigError> {
        let mut agent_names = HashSet::new();
        for agent in &self.agents {
            if !agent_names.insert(agent.name.as_str()) {
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
                if !judge_names.contains(judge_name.as_str()) {
                    return Err(ConfigError::JudgeNotConfigured {
                        agent: agent.name.clone(),
                        judge: judge_name.clone(),
                    });
                }
            }
        }
        Ok(agent_names)
    }

    fn validate_experiments(
        &self,
        agent_names: &HashSet<&str>,
    ) -> Result<HashSet<&str>, ConfigError> {
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
        Ok(experiment_names)
    }

    fn validate_judges(&self) -> Result<HashSet<&str>, ConfigError> {
        let mut judge_names = HashSet::new();
        for judge in &self.judges {
            if !judge_names.insert(judge.name.as_str()) {
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
        Ok(judge_names)
    }

    fn validate_smoke_tests(
        &self,
        agent_names: &HashSet<&str>,
        experiment_names: &HashSet<&str>,
    ) -> Result<(), ConfigError> {
        let mut smoke_names: HashSet<&str> = HashSet::new();
        for test in &self.smoke_tests {
            if !smoke_names.insert(test.name.as_str()) {
                return Err(ConfigError::DuplicateSmokeTest(test.name.clone()));
            }
            if test.max_turns == 0 {
                return Err(ConfigError::SmokeMaxTurnsZero(test.name.clone()));
            }
            if test.repetitions == 0 {
                return Err(ConfigError::SmokeRepetitionsZero(test.name.clone()));
            }
            let provider = ProviderKind::parse(&test.persona.provider).ok_or_else(|| {
                ConfigError::SmokePersonaUnknownProvider {
                    provider: test.persona.provider.clone(),
                    test: test.name.clone(),
                }
            })?;
            if !self.providers.contains_key(&provider) {
                return Err(ConfigError::SmokePersonaProviderNotConfigured {
                    provider,
                    test: test.name.clone(),
                });
            }
            if !agent_names.contains(test.target.as_str())
                && !experiment_names.contains(test.target.as_str())
            {
                return Err(ConfigError::SmokeUnknownTarget {
                    target: test.target.clone(),
                    test: test.name.clone(),
                });
            }
        }
        Ok(())
    }

    /// Subagent references resolve against the *combined* namespace of
    /// agents + experiments so an agent can list an experiment as a
    /// subagent.
    fn validate_subagents(
        &self,
        agent_names: &HashSet<&str>,
        experiment_names: &HashSet<&str>,
    ) -> Result<(), ConfigError> {
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
        Strategy::Bandit => validate_bandit_fields(config, experiment),
        Strategy::Shadow => validate_shadow_fields(experiment),
        Strategy::Split => validate_split_fields(experiment),
    }
}

fn validate_split_fields(experiment: &ExperimentConfig) -> Result<(), ConfigError> {
    reject_shadow_fields(experiment)?;
    reject_bandit_fields(experiment)?;
    Ok(())
}

fn validate_shadow_fields(experiment: &ExperimentConfig) -> Result<(), ConfigError> {
    reject_bandit_fields(experiment)?;
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
    Ok(())
}

fn validate_bandit_fields(
    config: &Config,
    experiment: &ExperimentConfig,
) -> Result<(), ConfigError> {
    reject_shadow_fields(experiment)?;
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
    Ok(())
}

/// Reject fields that only apply to the `shadow` strategy.
fn reject_shadow_fields(experiment: &ExperimentConfig) -> Result<(), ConfigError> {
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
    Ok(())
}

/// Reject fields that only apply to the `bandit` strategy.
fn reject_bandit_fields(experiment: &ExperimentConfig) -> Result<(), ConfigError> {
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
    #[error(transparent)]
    Auth(auth::ConfigError),
    #[error("experiment '{0}' uses strategy 'bandit' but does not declare a metric")]
    BanditWithoutMetric(String),
    #[error("default_user_id must be non-empty when set")]
    BlankDefaultUserId,
    #[error(
        "config variable '{var}' referenced via ${{vars.{var}}} is not declared in the `vars:` block\n  at {path}:{line_number}\n   | {line_content}\n   = help: add `{var}: ...` under the top-level `vars:` block"
    )]
    ConfigVarNotSet {
        line_content: String,
        line_number: usize,
        path: String,
        var: String,
    },
    #[error("duplicate agent name in config: {0}")]
    DuplicateAgent(String),
    #[error("duplicate judge name in config: {0}")]
    DuplicateJudge(String),
    #[error(
        "knowledge source '{name}' has an empty slug after normalization; use ASCII letters, digits, hyphens, or underscores"
    )]
    KnowledgeEmptySlug { name: String },
    #[error(
        "knowledge source '{name}' contains non-ASCII characters; use ASCII letters, digits, hyphens, underscores, slashes"
    )]
    KnowledgeNonAsciiName { name: String },
    #[error(
        "knowledge sources '{name_a}' and '{name_b}' both normalize to slug '{slug}'; rename one"
    )]
    KnowledgeSlugCollision {
        name_a: String,
        name_b: String,
        slug: String,
    },
    #[error(
        "knowledge source '{name}' normalizes to slug '{slug}' which makes tool name 'search_{slug}' exceed 64 characters"
    )]
    KnowledgeSlugTooLong { name: String, slug: String },
    #[error("duplicate sidecar name in config: {0}")]
    DuplicateSidecar(String),
    #[error("duplicate smoke test name in config: {0}")]
    DuplicateSmokeTest(String),
    #[error("agent '{agent}' lists subagent '{subagent}' more than once")]
    DuplicateSubagent { agent: String, subagent: String },
    #[error("duplicate trigger name in config: {0}")]
    DuplicateTrigger(String),
    #[error(
        "environment variable '{var}' referenced in config is not set\n  at {path}:{line_number}\n   | {line_content}\n   = help: export {var}=... in your shell before starting coulisse"
    )]
    EnvVarNotSet {
        line_content: String,
        line_number: usize,
        path: String,
        var: String,
    },
    #[error(
        "experiment '{0}' shares a name with an agent; rename one — experiment and agent names share a single namespace"
    )]
    ExperimentAgentNameCollision(String),
    #[error("experiment '{experiment}' lists variant agent '{agent}' more than once")]
    ExperimentDuplicateVariant { agent: String, experiment: String },
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
    #[error("experiment '{experiment}' has variant '{agent}' with non-positive weight {weight}")]
    ExperimentInvalidWeight {
        agent: String,
        experiment: String,
        weight: f32,
    },
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
    #[error("duplicate experiment name in config: {0}")]
    ExperimentNameCollision(String),
    #[error("experiment '{experiment}' has primary '{primary}' which is not one of its variants")]
    ExperimentPrimaryNotVariant { experiment: String, primary: String },
    #[error(
        "experiment '{experiment}' references variant agent '{agent}' which is not defined under `agents:`"
    )]
    ExperimentUnknownVariant { agent: String, experiment: String },
    #[error("experiment '{0}' must declare at least one variant")]
    ExperimentWithoutVariants(String),
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
    #[error("mcp server '{server}' has an oauth block but field '{field}' is blank")]
    McpOAuthBlankField { field: &'static str, server: String },
    #[error("at least one MCP server has an oauth block, but auth.mcp_consumer_secret is not set")]
    McpOAuthMissingConsumerSecret,
    #[error("COULISSE_HMAC_KEY env var must be set when any MCP server has an oauth block")]
    McpOAuthMissingHmacKey,
    #[error("COULISSE_VAULT_KEY env var must be set when any MCP server has an oauth block")]
    McpOAuthMissingVaultKey,
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
    #[error("experiment '{0}' uses strategy 'shadow' but does not declare a primary variant")]
    ShadowWithoutPrimary(String),
    #[error("sidecar '{0}' has a blank command — the executable to spawn is required")]
    SidecarBlankCommand(String),
    #[error("smoke test '{0}' has max_turns=0; set it to at least 1")]
    SmokeMaxTurnsZero(String),
    #[error(
        "smoke test '{test}' persona references provider '{provider}' which is not declared under `providers:`"
    )]
    SmokePersonaProviderNotConfigured {
        provider: ProviderKind,
        test: String,
    },
    #[error(
        "smoke test '{test}' persona provider '{provider}' is not supported (anthropic, cohere, deepseek, gemini, groq, openai)"
    )]
    SmokePersonaUnknownProvider { provider: String, test: String },
    #[error("smoke test '{0}' has repetitions=0; set it to at least 1")]
    SmokeRepetitionsZero(String),
    #[error("smoke test '{test}' targets '{target}' which is neither an agent nor an experiment")]
    SmokeUnknownTarget { target: String, test: String },
    #[error(transparent)]
    Trigger(triggers::TriggerError),
    #[error("trigger '{trigger}' targets '{agent}' which is neither an agent nor an experiment")]
    TriggerUnknownAgent { agent: String, trigger: String },
    #[error("trigger '{trigger}' webhook path '{path}' is already used by another trigger")]
    TriggerWebhookPathDuplicate { path: String, trigger: String },
    #[error(
        "trigger '{trigger}' webhook path '{path}' must start with '/hooks/' \
         (keeps webhook routes namespaced away from /v1, /admin, and /mcp)"
    )]
    TriggerWebhookPathInvalid { path: String, trigger: String },
    #[error("agent '{agent}' references subagent '{subagent}' which is not defined")]
    UnknownSubagent { agent: String, subagent: String },
    #[error(
        "unclosed '${{' in config — every '${{' must have a matching '}}'\n  at {path}:{line_number}\n   | {line_content}"
    )]
    UnclosedEnvVar {
        line_content: String,
        line_number: usize,
        path: String,
    },
}

/// Errors raised while expanding `${VAR}` placeholders in the raw YAML
/// text. Internal — `Config::from_path` enriches these into the
/// path/line-aware `ConfigError::EnvVarNotSet` and `UnclosedEnvVar`.
#[derive(Debug, Error)]
enum ExpandError {
    #[error("config variable 'vars.{var}' is not declared")]
    ConfigVarNotSet { offset: usize, var: String },
    #[error("environment variable '{var}' is not set")]
    EnvVarNotSet { offset: usize, var: String },
    #[error("unclosed '${{'  in config")]
    UnclosedEnvVar { offset: usize },
}

impl ExpandError {
    fn offset(&self) -> usize {
        match self {
            Self::ConfigVarNotSet { offset, .. }
            | Self::EnvVarNotSet { offset, .. }
            | Self::UnclosedEnvVar { offset } => *offset,
        }
    }
}
#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<Config, ConfigError> {
        let config: Config = serde_yaml::from_str(yaml).map_err(ConfigError::ParseConfig)?;
        config.validate()?;
        Ok(config)
    }

    const BASE_PROVIDERS: &str = r"
providers:
  openai:
    api_key: test
";

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
                assert_eq!(primary, "alice-v2");
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
                assert_eq!(field, "primary");
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
                assert_eq!(judge, "ghost");
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
                assert_eq!(criterion, "tone");
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
                assert_eq!(agent, "alice-v2");
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

    const SMOKE_BASE: &str = r"
providers:
  openai:
    api_key: test
agents:
  - name: assistant
    provider: openai
    model: gpt-4
";

    #[test]
    fn smoke_test_with_valid_target_parses() {
        let yaml = format!(
            "{SMOKE_BASE}smoke_tests:
  - name: smoke_one
    target: assistant
    persona:
      provider: openai
      model: gpt-4
      preamble: You are a curious user.
"
        );
        let config = parse(&yaml).expect("valid smoke test");
        assert_eq!(config.smoke_tests.len(), 1);
        assert_eq!(config.smoke_tests[0].name, "smoke_one");
        assert_eq!(config.smoke_tests[0].max_turns, 10);
        assert_eq!(config.smoke_tests[0].repetitions, 1);
    }

    #[test]
    fn smoke_test_targeting_experiment_validates() {
        let yaml = format!(
            "{SMOKE_BASE}  - name: assistant-v2
    provider: openai
    model: gpt-4
experiments:
  - name: rollout
    strategy: split
    variants:
      - agent: assistant
      - agent: assistant-v2
smoke_tests:
  - name: rollout_check
    target: rollout
    persona:
      provider: openai
      model: gpt-4
      preamble: Ask one question.
"
        );
        parse(&yaml).expect("experiment as smoke target should validate");
    }

    #[test]
    fn smoke_test_with_unknown_target_is_rejected() {
        let yaml = format!(
            "{SMOKE_BASE}smoke_tests:
  - name: ghost
    target: missing
    persona:
      provider: openai
      model: gpt-4
      preamble: x
"
        );
        match parse(&yaml) {
            Err(ConfigError::SmokeUnknownTarget { target, test }) => {
                assert_eq!(target, "missing");
                assert_eq!(test, "ghost");
            }
            other => panic!("expected SmokeUnknownTarget, got {other:?}"),
        }
    }

    #[test]
    fn smoke_test_with_unconfigured_persona_provider_is_rejected() {
        let yaml = format!(
            "{SMOKE_BASE}smoke_tests:
  - name: missing_provider
    target: assistant
    persona:
      provider: anthropic
      model: claude
      preamble: x
"
        );
        match parse(&yaml) {
            Err(ConfigError::SmokePersonaProviderNotConfigured { provider, test }) => {
                assert_eq!(provider, ProviderKind::Anthropic);
                assert_eq!(test, "missing_provider");
            }
            other => panic!("expected SmokePersonaProviderNotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn smoke_test_with_unknown_persona_provider_is_rejected() {
        let yaml = format!(
            "{SMOKE_BASE}smoke_tests:
  - name: bogus_provider
    target: assistant
    persona:
      provider: not-a-provider
      model: x
      preamble: x
"
        );
        match parse(&yaml) {
            Err(ConfigError::SmokePersonaUnknownProvider { provider, test }) => {
                assert_eq!(provider, "not-a-provider");
                assert_eq!(test, "bogus_provider");
            }
            other => panic!("expected SmokePersonaUnknownProvider, got {other:?}"),
        }
    }

    #[test]
    fn smoke_test_with_zero_max_turns_is_rejected() {
        let yaml = format!(
            "{SMOKE_BASE}smoke_tests:
  - name: empty
    target: assistant
    max_turns: 0
    persona:
      provider: openai
      model: gpt-4
      preamble: x
"
        );
        match parse(&yaml) {
            Err(ConfigError::SmokeMaxTurnsZero(name)) => assert_eq!(name, "empty"),
            other => panic!("expected SmokeMaxTurnsZero, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_smoke_tests_are_rejected() {
        let yaml = format!(
            "{SMOKE_BASE}smoke_tests:
  - name: same
    target: assistant
    persona:
      provider: openai
      model: gpt-4
      preamble: a
  - name: same
    target: assistant
    persona:
      provider: openai
      model: gpt-4
      preamble: b
"
        );
        match parse(&yaml) {
            Err(ConfigError::DuplicateSmokeTest(name)) => assert_eq!(name, "same"),
            other => panic!("expected DuplicateSmokeTest, got {other:?}"),
        }
    }

    fn lookup(var: &str) -> Option<String> {
        match var {
            "KEY" => Some("hello".into()),
            "A" => Some("foo".into()),
            "B" => Some("bar".into()),
            _ => None,
        }
    }

    #[test]
    fn expand_env_vars_substitutes_set_variables() {
        let result = expand_env_vars_with("prefix_${KEY}_suffix", lookup).unwrap();
        assert_eq!(result, "prefix_hello_suffix");
    }

    #[test]
    fn expand_env_vars_multiple_vars_in_one_string() {
        let result = expand_env_vars_with("${A}:${B}", lookup).unwrap();
        assert_eq!(result, "foo:bar");
    }

    #[test]
    fn expand_env_vars_no_placeholders_returns_input_unchanged() {
        let result = expand_env_vars_with("no variables here", lookup).unwrap();
        assert_eq!(result, "no variables here");
    }

    #[test]
    fn expand_env_vars_unset_variable_errors() {
        match expand_env_vars_with("${MISSING}", lookup) {
            Err(ExpandError::EnvVarNotSet { var, offset }) => {
                assert_eq!(var, "MISSING");
                assert_eq!(offset, 0);
            }
            other => panic!("expected EnvVarNotSet, got {other:?}"),
        }
    }

    #[test]
    fn expand_env_vars_unclosed_brace_errors() {
        match expand_env_vars_with("${UNCLOSED", lookup) {
            Err(ExpandError::UnclosedEnvVar { offset }) => assert_eq!(offset, 0),
            other => panic!("expected UnclosedEnvVar, got {other:?}"),
        }
    }

    #[test]
    fn expand_env_vars_records_offset_on_third_line() {
        let source = "line1: a\nline2: b\nline3: ${MISSING}\n";
        match expand_env_vars_with(source, lookup) {
            Err(ExpandError::EnvVarNotSet { var, offset }) => {
                assert_eq!(var, "MISSING");
                let (line_number, line_content) = locate(source, offset);
                assert_eq!(line_number, 3);
                assert_eq!(line_content, "line3: ${MISSING}");
            }
            other => panic!("expected EnvVarNotSet, got {other:?}"),
        }
    }

    #[test]
    fn locate_handles_offset_past_end() {
        let source = "one\ntwo\nthree";
        let (line_number, line_content) = locate(source, source.len() + 100);
        assert_eq!(line_number, 3);
        assert_eq!(line_content, "three");
    }

    #[test]
    fn env_var_pass_leaves_config_vars_intact() {
        let result = expand_env_vars_with("hello ${vars.team} ${KEY}", lookup).unwrap();
        assert_eq!(result, "hello ${vars.team} hello");
    }

    #[test]
    fn config_var_pass_substitutes_known_vars() {
        let mut vars = HashMap::new();
        vars.insert("team".to_string(), "alice, bob".to_string());
        let result = expand_config_vars("members: ${vars.team}", &vars).unwrap();
        assert_eq!(result, "members: alice, bob");
    }

    #[test]
    fn config_var_pass_errors_on_unknown_var() {
        let vars = HashMap::new();
        match expand_config_vars("${vars.ghost}", &vars) {
            Err(ExpandError::ConfigVarNotSet { var, offset }) => {
                assert_eq!(var, "ghost");
                assert_eq!(offset, 0);
            }
            other => panic!("expected ConfigVarNotSet, got {other:?}"),
        }
    }

    #[test]
    fn config_var_pass_preserves_indent_for_multiline_values() {
        let mut vars = HashMap::new();
        vars.insert(
            "rooms".to_string(),
            "**Rooms:**\n- #standup\n- #engineering".to_string(),
        );
        // 6 spaces of leading indent — matches what a YAML block scalar
        // would carry.
        let src = "      ${vars.rooms}";
        let out = expand_config_vars(src, &vars).unwrap();
        assert_eq!(
            out,
            "      **Rooms:**\n      - #standup\n      - #engineering"
        );
    }

    #[test]
    fn config_var_pass_does_not_indent_empty_lines() {
        let mut vars = HashMap::new();
        vars.insert("snippet".to_string(), "line one\n\nline three".to_string());
        let out = expand_config_vars("    ${vars.snippet}", &vars).unwrap();
        // Empty lines should not get indent — they stay blank.
        assert_eq!(out, "    line one\n\n    line three");
    }

    #[test]
    fn config_var_pass_does_not_recurse() {
        // A var's value is substituted verbatim. If the value itself contains
        // `${vars.x}`, that text is left as-is — single-pass by design.
        let mut vars = HashMap::new();
        vars.insert("a".to_string(), "before ${vars.b} after".to_string());
        vars.insert("b".to_string(), "B".to_string());
        let result = expand_config_vars("${vars.a}", &vars).unwrap();
        assert_eq!(result, "before ${vars.b} after");
    }

    #[test]
    fn config_var_pass_handles_unclosed_brace() {
        let vars = HashMap::new();
        match expand_config_vars("${vars.unterminated", &vars) {
            Err(ExpandError::UnclosedEnvVar { offset }) => assert_eq!(offset, 0),
            other => panic!("expected UnclosedEnvVar, got {other:?}"),
        }
    }

    // ── to_slug ──────────────────────────────────────────────────────────────────────

    #[test]
    fn to_slug_hyphens_become_underscores() {
        assert_eq!(to_slug("rust-book").unwrap(), "rust_book");
    }

    #[test]
    fn to_slug_mixed_case_is_lowercased() {
        assert_eq!(to_slug("Rust-Book").unwrap(), "rust_book");
    }

    #[test]
    fn to_slug_slashes_become_underscores() {
        assert_eq!(to_slug("docs/v2/api").unwrap(), "docs_v2_api");
    }

    #[test]
    fn to_slug_consecutive_underscores_collapsed() {
        assert_eq!(to_slug("__foo__").unwrap(), "foo");
        assert_eq!(to_slug("foo__bar").unwrap(), "foo_bar");
    }

    #[test]
    fn to_slug_spaces_and_special_chars_produce_empty_after_trim() {
        assert_eq!(to_slug(" ").unwrap(), "");
        assert_eq!(to_slug("---").unwrap(), "");
    }

    #[test]
    fn to_slug_non_ascii_is_rejected() {
        match to_slug("café") {
            Err(SlugError::NonAscii) => {}
            other => panic!("expected NonAscii, got {other:?}"),
        }
    }

    #[test]
    fn to_slug_all_lower_alphanumeric_unchanged() {
        assert_eq!(to_slug("already_fine").unwrap(), "already_fine");
    }

    // ── validate_knowledge ─────────────────────────────────────────────────────────

    const KNOWLEDGE_BASE: &str = r"
providers:
  openai:
    api_key: test
agents:
  - name: assistant
    provider: openai
    model: gpt-4
";

    #[test]
    fn knowledge_source_with_valid_name_parses() {
        let yaml = format!(
            "{KNOWLEDGE_BASE}knowledge:
  - name: rust-book
    source: ./docs/rust
"
        );
        let config = parse(&yaml).expect("valid knowledge source");
        assert_eq!(config.knowledge.len(), 1);
        assert_eq!(config.knowledge[0].name.as_deref(), Some("rust-book"));
    }

    #[test]
    fn knowledge_empty_slug_is_rejected() {
        let yaml = format!(
            "{KNOWLEDGE_BASE}knowledge:
  - name: '---'
    source: ./docs
"
        );
        match parse(&yaml) {
            Err(ConfigError::KnowledgeEmptySlug { name }) => {
                assert_eq!(name, "---");
            }
            other => panic!("expected KnowledgeEmptySlug, got {other:?}"),
        }
    }

    #[test]
    fn knowledge_slug_too_long_is_rejected() {
        // 58-char slug → tool name would be 65 chars, over the 64-char limit.
        let long_name = "a".repeat(58);
        let yaml = format!(
            "{KNOWLEDGE_BASE}knowledge:
  - name: {long_name}
    source: ./docs
"
        );
        match parse(&yaml) {
            Err(ConfigError::KnowledgeSlugTooLong { name, slug }) => {
                assert_eq!(name, long_name);
                assert_eq!(slug.len(), 58);
            }
            other => panic!("expected KnowledgeSlugTooLong, got {other:?}"),
        }
    }

    #[test]
    fn knowledge_slug_exactly_57_chars_is_accepted() {
        let name_57 = "a".repeat(57);
        let yaml = format!(
            "{KNOWLEDGE_BASE}knowledge:
  - name: {name_57}
    source: ./docs
"
        );
        parse(&yaml).expect("57-char slug should be accepted");
    }

    #[test]
    fn knowledge_non_ascii_name_is_rejected() {
        let yaml = format!(
            "{KNOWLEDGE_BASE}knowledge:
  - name: \"café\"
    source: ./docs
"
        );
        match parse(&yaml) {
            Err(ConfigError::KnowledgeNonAsciiName { name }) => {
                assert_eq!(name, "café");
            }
            other => panic!("expected KnowledgeNonAsciiName, got {other:?}"),
        }
    }

    #[test]
    fn knowledge_slug_collision_order_a_is_rejected() {
        // rust-book → rust_book, then rust_book → rust_book: collision.
        let yaml = format!(
            "{KNOWLEDGE_BASE}knowledge:
  - name: rust-book
    source: ./docs/a
  - name: rust_book
    source: ./docs/b
"
        );
        match parse(&yaml) {
            Err(ConfigError::KnowledgeSlugCollision {
                name_a,
                name_b,
                slug,
            }) => {
                assert_eq!(name_a, "rust-book");
                assert_eq!(name_b, "rust_book");
                assert_eq!(slug, "rust_book");
            }
            other => panic!("expected KnowledgeSlugCollision, got {other:?}"),
        }
    }

    #[test]
    fn knowledge_slug_collision_order_b_is_rejected() {
        // rust_book first, then rust-book → same slug: collision regardless of order.
        let yaml = format!(
            "{KNOWLEDGE_BASE}knowledge:
  - name: rust_book
    source: ./docs/a
  - name: rust-book
    source: ./docs/b
"
        );
        match parse(&yaml) {
            Err(ConfigError::KnowledgeSlugCollision {
                name_a,
                name_b,
                slug,
            }) => {
                assert_eq!(name_a, "rust_book");
                assert_eq!(name_b, "rust-book");
                assert_eq!(slug, "rust_book");
            }
            other => panic!("expected KnowledgeSlugCollision, got {other:?}"),
        }
    }

    #[test]
    fn knowledge_spaces_only_name_produces_empty_slug() {
        let yaml = format!(
            "{KNOWLEDGE_BASE}knowledge:
  - name: \"   \"
    source: ./docs
"
        );
        match parse(&yaml) {
            Err(ConfigError::KnowledgeEmptySlug { .. }) => {}
            other => panic!("expected KnowledgeEmptySlug, got {other:?}"),
        }
    }

    #[test]
    fn config_var_in_preamble_resolves_through_from_str() {
        let yaml = "
vars:
  team_footer: |
    Team: @pm, @coder, @qa
providers:
  openai:
    api_key: test
agents:
  - name: pm
    provider: openai
    model: gpt-4
    preamble: |
      You are the PM.
      ${vars.team_footer}
";
        let config = Config::from_str(yaml, "<test>").expect("valid config with vars");
        let pm = config.agents.iter().find(|a| a.name == "pm").unwrap();
        assert!(
            pm.preamble.contains("Team: @pm, @coder, @qa"),
            "preamble should contain substituted team footer, got: {:?}",
            pm.preamble
        );
    }

    #[test]
    fn unknown_config_var_errors_with_line_context() {
        let yaml = "
providers:
  openai:
    api_key: test
agents:
  - name: pm
    provider: openai
    model: gpt-4
    preamble: ${vars.ghost}
";
        match Config::from_str(yaml, "<test>") {
            Err(ConfigError::ConfigVarNotSet {
                var,
                line_number,
                line_content,
                ..
            }) => {
                assert_eq!(var, "ghost");
                assert!(line_number > 0);
                assert!(
                    line_content.contains("${vars.ghost}"),
                    "line content should include the offending placeholder, got: {line_content:?}"
                );
            }
            other => panic!("expected ConfigVarNotSet, got {other:?}"),
        }
    }

    // WHY: the mcp_oauth_* tests below mutate process-global env vars
    // (COULISSE_VAULT_KEY, COULISSE_HMAC_KEY) which Rust's parallel
    // test runner can race on. Acquire this mutex at the top of each
    // test that touches those vars to serialize them.
    static OAUTH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const MCP_OAUTH_BASE: &str = r"
providers:
  openai:
    api_key: test
agents:
  - name: assistant
    provider: openai
    model: gpt-4
auth:
  mcp_consumer_secret: s3cr3t
mcp:
  jira:
    transport: http
    url: https://mcp.example.com
    oauth:
      authorization_url: https://auth.example.com/authorize
      client_id: client-id
      client_secret: client-secret
      redirect_uri: https://coulisse.example.com/mcp/jira/oauth/callback
      token_url: https://auth.example.com/oauth/token
";

    #[test]
    fn mcp_oauth_missing_consumer_secret_is_rejected() {
        let yaml = r"
providers:
  openai:
    api_key: test
agents:
  - name: assistant
    provider: openai
    model: gpt-4
mcp:
  jira:
    transport: http
    url: https://mcp.example.com
    oauth:
      authorization_url: https://auth.example.com/authorize
      client_id: client-id
      client_secret: client-secret
      redirect_uri: https://coulisse.example.com/mcp/jira/oauth/callback
      token_url: https://auth.example.com/oauth/token
";
        let config: Config = serde_yaml::from_str(yaml).expect("parses");
        match config.validate() {
            Err(ConfigError::McpOAuthMissingConsumerSecret) => {}
            other => panic!("expected McpOAuthMissingConsumerSecret, got {other:?}"),
        }
    }

    #[test]
    fn mcp_oauth_missing_vault_key_is_rejected() {
        let _guard = OAUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Ensure COULISSE_VAULT_KEY is not set for this test.
        unsafe {
            std::env::remove_var("COULISSE_VAULT_KEY");
            std::env::remove_var("COULISSE_HMAC_KEY");
        }
        let config: Config = serde_yaml::from_str(MCP_OAUTH_BASE).expect("parses");
        match config.validate() {
            Err(ConfigError::McpOAuthMissingVaultKey) => {}
            other => panic!("expected McpOAuthMissingVaultKey, got {other:?}"),
        }
    }

    #[test]
    fn mcp_oauth_missing_hmac_key_is_rejected() {
        let _guard = OAUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        unsafe {
            std::env::set_var(
                "COULISSE_VAULT_KEY",
                "dGVzdC10ZXN0LXRlc3QtdGVzdC10ZXN0LXRlc3Q=",
            );
            std::env::remove_var("COULISSE_HMAC_KEY");
        }
        let config: Config = serde_yaml::from_str(MCP_OAUTH_BASE).expect("parses");
        let result = config.validate();
        unsafe {
            std::env::remove_var("COULISSE_VAULT_KEY");
        }
        match result {
            Err(ConfigError::McpOAuthMissingHmacKey) => {}
            other => panic!("expected McpOAuthMissingHmacKey, got {other:?}"),
        }
    }

    #[test]
    fn mcp_oauth_blank_field_is_rejected() {
        let _guard = OAUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        unsafe {
            std::env::set_var(
                "COULISSE_VAULT_KEY",
                "dGVzdC10ZXN0LXRlc3QtdGVzdC10ZXN0LXRlc3Q=",
            );
            std::env::set_var(
                "COULISSE_HMAC_KEY",
                "dGVzdC10ZXN0LXRlc3QtdGVzdC10ZXN0LXRlc3Q=",
            );
        }
        let yaml = r#"
providers:
  openai:
    api_key: test
agents:
  - name: assistant
    provider: openai
    model: gpt-4
auth:
  mcp_consumer_secret: s3cr3t
mcp:
  jira:
    transport: http
    url: https://mcp.example.com
    oauth:
      authorization_url: ""
      client_id: client-id
      client_secret: client-secret
      redirect_uri: https://coulisse.example.com/mcp/jira/oauth/callback
      token_url: https://auth.example.com/oauth/token
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("parses");
        let result = config.validate();
        unsafe {
            std::env::remove_var("COULISSE_VAULT_KEY");
            std::env::remove_var("COULISSE_HMAC_KEY");
        }
        match result {
            Err(ConfigError::McpOAuthBlankField { field, server }) => {
                assert_eq!(field, "authorization_url");
                assert_eq!(server, "jira");
            }
            other => panic!("expected McpOAuthBlankField, got {other:?}"),
        }
    }
}
