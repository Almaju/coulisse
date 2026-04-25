use std::collections::HashSet;

use crate::{Config, ConfigError, ProviderKind};

impl Config {
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
        for agent in &self.agents {
            let mut sub_seen = HashSet::new();
            for sub in &agent.subagents {
                if sub == &agent.name {
                    return Err(ConfigError::SelfSubagent(agent.name.clone()));
                }
                if !agent_names.contains(sub.as_str()) {
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
