use std::collections::{BTreeMap, HashMap};
use std::{fs, path::Path};

pub use experiments::{ExperimentConfig, Strategy, Variant};
use memory::MemoryConfig;
use serde::Deserialize;

use crate::ConfigError;

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
}

/// Authentication for the studio UI and its JSON API. Exactly one of
/// `basic` or `oidc` must be set — they are mutually exclusive so the
/// server never has to choose between two competing session schemes.
#[derive(Clone, Debug, Deserialize)]
pub struct StudioConfig {
    #[serde(default)]
    pub basic: Option<StudioBasicConfig>,
    #[serde(default)]
    pub oidc: Option<StudioOidcConfig>,
}

/// Static HTTP Basic credentials. Appropriate for local dev or a
/// single-operator deployment. Browsers prompt via the native login
/// dialog; no session state.
#[derive(Clone, Debug, Deserialize)]
pub struct StudioBasicConfig {
    pub password: String,
    #[serde(default = "default_studio_username")]
    pub username: String,
}

/// OIDC (OpenID Connect) login. Validated against any compliant IdP —
/// Authentik, Keycloak, Auth0, Google, Microsoft, Okta. Access control
/// (who may use the studio) is delegated to the IdP's application
/// bindings, not configured here.
#[derive(Clone, Debug, Deserialize)]
pub struct StudioOidcConfig {
    pub client_id: String,
    /// Optional for public clients that use PKCE only. Authentik's default
    /// "confidential" client type requires a secret.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// OIDC issuer URL. For Authentik, typically
    /// `https://authentik.example.com/application/o/<app-slug>/`.
    pub issuer_url: String,
    /// Absolute URL the IdP will redirect to after login. Must be
    /// whitelisted in the IdP's client config. The callback handler is
    /// served by Coulisse under this path; point it at a path inside
    /// `/studio/` (e.g. `https://coulisse.example.com/studio/auth/callback`).
    pub redirect_url: String,
    /// Additional OAuth2 scopes beyond the implicit `openid`. Defaults to
    /// `profile` and `email`; add `groups` if you want to surface group
    /// membership claims from Authentik (currently unused for authz, but
    /// available to future features).
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
}

fn default_studio_username() -> String {
    "admin".to_string()
}

fn default_oidc_scopes() -> Vec<String> {
    vec!["email".to_string(), "profile".to_string()]
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentConfig {
    /// Names of judges (defined at the top level under `judges:`) that should
    /// evaluate this agent's replies. Empty = no automatic evaluation.
    #[serde(default)]
    pub judges: Vec<String>,
    #[serde(default)]
    pub mcp_tools: Vec<McpToolAccess>,
    pub model: String,
    pub name: String,
    #[serde(default)]
    pub preamble: String,
    pub provider: ProviderKind,
    /// Short description used as the tool description when this agent is
    /// exposed to other agents via `subagents:`. If absent, the agent's
    /// `name` is used as a fallback — but clear prose here helps the caller
    /// LLM decide when to invoke this agent.
    #[serde(default)]
    pub purpose: Option<String>,
    /// Other agents exposed to this agent as tools. Names must match entries
    /// in the top-level `agents:` list. Self-reference is rejected; duplicate
    /// entries are rejected. Calling a subagent runs a fresh conversation
    /// against that agent's preamble + MCP tools; the subagent's final
    /// message is returned as the tool result.
    #[serde(default)]
    pub subagents: Vec<String>,
}

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

#[derive(Clone, Debug, Deserialize)]
pub struct McpToolAccess {
    #[serde(default)]
    pub only: Option<Vec<String>>,
    pub server: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpServerConfig {
    Http {
        url: String,
    },
    Stdio {
        #[serde(default)]
        args: Vec<String>,
        command: String,
        #[serde(default)]
        env: HashMap<String, String>,
    },
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    pub api_key: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    Cohere,
    Deepseek,
    Gemini,
    Groq,
    Openai,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Cohere => "cohere",
            Self::Deepseek => "deepseek",
            Self::Gemini => "gemini",
            Self::Groq => "groq",
            Self::Openai => "openai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "cohere" => Some(Self::Cohere),
            "deepseek" => Some(Self::Deepseek),
            "gemini" => Some(Self::Gemini),
            "groq" => Some(Self::Groq),
            "openai" => Some(Self::Openai),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
