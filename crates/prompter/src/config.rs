use std::collections::{BTreeMap, HashMap, HashSet};
use std::{fs, path::Path};

use memory::MemoryConfig;
use serde::Deserialize;

use crate::PrompterError;

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
}

impl Config {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, PrompterError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| PrompterError::ReadConfig {
            path: path.display().to_string(),
            source,
        })?;
        let config: Self = serde_yaml::from_str(&contents).map_err(PrompterError::ParseConfig)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), PrompterError> {
        if self.agents.is_empty() {
            return Err(PrompterError::NoAgents);
        }
        if let Some(id) = &self.default_user_id
            && id.trim().is_empty()
        {
            return Err(PrompterError::BlankDefaultUserId);
        }
        let mut judge_names = HashSet::new();
        for judge in &self.judges {
            if !judge_names.insert(&judge.name) {
                return Err(PrompterError::DuplicateJudge(judge.name.clone()));
            }
            if judge.rubrics.is_empty() {
                return Err(PrompterError::JudgeWithoutRubrics(judge.name.clone()));
            }
            if !(0.0..=1.0).contains(&judge.sampling_rate) {
                return Err(PrompterError::InvalidSamplingRate {
                    judge: judge.name.clone(),
                    value: judge.sampling_rate,
                });
            }
            let provider = ProviderKind::parse(&judge.provider).ok_or_else(|| {
                PrompterError::JudgeUnknownProvider {
                    judge: judge.name.clone(),
                    provider: judge.provider.clone(),
                }
            })?;
            if !self.providers.contains_key(&provider) {
                return Err(PrompterError::JudgeProviderNotConfigured {
                    judge: judge.name.clone(),
                    provider,
                });
            }
        }
        let mut seen = HashSet::new();
        for agent in &self.agents {
            if !seen.insert(&agent.name) {
                return Err(PrompterError::DuplicateAgent(agent.name.clone()));
            }
            if !self.providers.contains_key(&agent.provider) {
                return Err(PrompterError::ProviderNotConfigured {
                    agent: agent.name.clone(),
                    provider: agent.provider,
                });
            }
            for access in &agent.mcp_tools {
                if !self.mcp.contains_key(&access.server) {
                    return Err(PrompterError::McpServerNotConfigured {
                        agent: agent.name.clone(),
                        server: access.server.clone(),
                    });
                }
            }
            for judge_name in &agent.judges {
                if !judge_names.contains(judge_name) {
                    return Err(PrompterError::JudgeNotConfigured {
                        agent: agent.name.clone(),
                        judge: judge_name.clone(),
                    });
                }
            }
        }
        let agent_names: HashSet<&str> = self.agents.iter().map(|a| a.name.as_str()).collect();
        for agent in &self.agents {
            let mut sub_seen = HashSet::new();
            for sub in &agent.subagents {
                if sub == &agent.name {
                    return Err(PrompterError::SelfSubagent(agent.name.clone()));
                }
                if !agent_names.contains(sub.as_str()) {
                    return Err(PrompterError::UnknownSubagent {
                        agent: agent.name.clone(),
                        subagent: sub.clone(),
                    });
                }
                if !sub_seen.insert(sub) {
                    return Err(PrompterError::DuplicateSubagent {
                        agent: agent.name.clone(),
                        subagent: sub.clone(),
                    });
                }
            }
        }
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<Config, PrompterError> {
        let config: Config = serde_yaml::from_str(yaml).map_err(PrompterError::ParseConfig)?;
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
            Err(PrompterError::SelfSubagent(name)) => assert_eq!(name, "loopy"),
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
            Err(PrompterError::UnknownSubagent { agent, subagent }) => {
                assert_eq!(agent, "coach");
                assert_eq!(subagent, "ghost");
            }
            other => panic!("expected UnknownSubagent error, got {other:?}"),
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
            Err(PrompterError::DuplicateSubagent { agent, subagent }) => {
                assert_eq!(agent, "coach");
                assert_eq!(subagent, "helper");
            }
            other => panic!("expected DuplicateSubagent error, got {other:?}"),
        }
    }
}
