use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use config::{JudgeConfig, ProviderKind};
use memory::{MessageId, Score, Store, UserId};
use prompter::{Message as PrompterMessage, Prompter, Role as PrompterRole};
use serde::Deserialize;

/// Runtime judge built from YAML and validated at startup. Holds the
/// prebuilt preamble so the hot path does zero string construction before
/// hitting the model.
#[derive(Debug)]
pub struct Judge {
    pub criteria: Vec<String>,
    pub model: String,
    pub name: String,
    pub preamble: String,
    pub provider: ProviderKind,
    pub sampling_rate: f32,
}

impl Judge {
    /// Validate a `JudgeConfig` and produce a ready-to-run `Judge`. All
    /// shape/coverage checks that can fail belong at startup — the request
    /// path should only see known-good judges.
    pub fn from_config(config: &JudgeConfig) -> Result<Self, JudgeBuildError> {
        if config.rubrics.is_empty() {
            return Err(JudgeBuildError::NoRubrics {
                judge: config.name.clone(),
            });
        }
        if !(0.0..=1.0).contains(&config.sampling_rate) {
            return Err(JudgeBuildError::InvalidSamplingRate {
                judge: config.name.clone(),
                value: config.sampling_rate,
            });
        }
        let provider = ProviderKind::parse(&config.provider).ok_or_else(|| {
            JudgeBuildError::UnknownProvider {
                judge: config.name.clone(),
                provider: config.provider.clone(),
            }
        })?;
        let criteria: Vec<String> = config.rubrics.keys().cloned().collect();
        Ok(Self {
            preamble: build_preamble(&config.rubrics),
            criteria,
            model: config.model.clone(),
            name: config.name.clone(),
            provider,
            sampling_rate: config.sampling_rate,
        })
    }

    /// Draw once against the configured sampling rate. Called per scored
    /// turn so that across many turns the scored fraction converges on
    /// `sampling_rate`.
    pub fn should_sample(&self) -> bool {
        if self.sampling_rate >= 1.0 {
            return true;
        }
        if self.sampling_rate <= 0.0 {
            return false;
        }
        rand::random::<f32>() < self.sampling_rate
    }
}

/// Spawn a background task that runs each supplied judge against the last
/// exchange and persists scores. Sampling decisions happen per-judge inside
/// the task. Failures are logged and swallowed so the response path is
/// never affected.
pub fn spawn_score<P: Prompter + 'static>(
    judges: Vec<Arc<Judge>>,
    memory: Arc<Store>,
    prompter: Arc<P>,
    user_id: UserId,
    message_id: MessageId,
    user_message: String,
    assistant_message: String,
) {
    if judges.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for judge in judges {
            if !judge.should_sample() {
                continue;
            }
            if let Err(err) = run_score(
                &judge,
                &memory,
                prompter.as_ref(),
                user_id,
                message_id,
                &user_message,
                &assistant_message,
            )
            .await
            {
                tracing::warn!(
                    user = %user_id.0,
                    judge = %judge.name,
                    error = %err,
                    "judge scoring failed",
                );
            }
        }
    });
}

async fn run_score<P: Prompter>(
    judge: &Judge,
    memory: &Store,
    prompter: &P,
    user_id: UserId,
    message_id: MessageId,
    user_message: &str,
    assistant_message: &str,
) -> Result<(), JudgeRunError> {
    let turn = PrompterMessage {
        content: format!(
            "User message:\n{user_message}\n\nAssistant reply:\n{assistant_message}\n\nReturn the JSON object now."
        ),
        role: PrompterRole::User,
    };
    let completion = prompter
        .prompt_with(judge.provider, &judge.model, &judge.preamble, vec![turn])
        .await
        .map_err(JudgeRunError::Prompt)?;
    let raw = parse_scores(&completion.text).map_err(JudgeRunError::Parse)?;
    let um = memory.for_user(user_id);
    for criterion in &judge.criteria {
        let Some(raw_score) = raw.get(criterion) else {
            tracing::debug!(
                judge = %judge.name,
                %criterion,
                "judge response omitted criterion — skipping",
            );
            continue;
        };
        let score = Score::new(
            user_id,
            message_id,
            judge.name.clone(),
            judge.model.clone(),
            criterion.clone(),
            clamp_score(raw_score.score),
            raw_score.reasoning.clone(),
        );
        if let Err(err) = um.append_score(score).await {
            tracing::warn!(
                judge = %judge.name,
                %criterion,
                error = %err,
                "failed to persist score",
            );
        }
    }
    Ok(())
}

fn build_preamble(rubrics: &BTreeMap<String, String>) -> String {
    let mut out = String::from(
        "You are an evaluation judge. Score the assistant's reply against each \
         criterion below on an integer scale from 0 (worst) to 10 (best), with a \
         concise one-sentence reasoning.\n\nCriteria:\n",
    );
    for (name, description) in rubrics {
        out.push_str(&format!("- {name}: {description}\n"));
    }
    out.push_str(
        "\nRespond ONLY with a JSON object whose top-level keys are the exact \
         criterion names listed above. Each value must be an object of the form \
         {\"score\": <integer 0-10>, \"reasoning\": <short string>}. Do not include \
         prose, markdown, or code fences — only the JSON object.",
    );
    out
}

fn parse_scores(text: &str) -> Result<HashMap<String, RawScore>, String> {
    let trimmed = strip_code_fence(text.trim()).trim();
    if trimmed.is_empty() {
        return Err("judge returned empty output".into());
    }
    serde_json::from_str(trimmed)
        .map_err(|e| format!("judge returned non-JSON output ({e}): {trimmed:?}"))
}

fn strip_code_fence(s: &str) -> &str {
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    let s = s.trim_start();
    s.strip_suffix("```").unwrap_or(s)
}

fn clamp_score(value: f32) -> f32 {
    if value.is_nan() {
        return 0.0;
    }
    value.clamp(0.0, 10.0)
}

#[derive(Debug, Deserialize)]
struct RawScore {
    reasoning: String,
    score: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum JudgeBuildError {
    #[error("judge '{judge}' sampling_rate={value} is outside [0.0, 1.0]")]
    InvalidSamplingRate { judge: String, value: f32 },
    #[error("judge '{judge}' must declare at least one rubric")]
    NoRubrics { judge: String },
    #[error(
        "judge '{judge}' provider '{provider}' is not supported (anthropic, cohere, deepseek, gemini, groq, openai)"
    )]
    UnknownProvider { judge: String, provider: String },
}

#[derive(Debug, thiserror::Error)]
enum JudgeRunError {
    #[error("parse: {0}")]
    Parse(String),
    #[error("prompt: {0}")]
    Prompt(prompter::PrompterError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(rubrics: Vec<(&str, &str)>, sampling_rate: f32) -> JudgeConfig {
        JudgeConfig {
            model: "test-model".into(),
            name: "test".into(),
            provider: "openai".into(),
            rubrics: rubrics
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            sampling_rate,
        }
    }

    #[test]
    fn from_config_rejects_empty_rubrics() {
        let err = Judge::from_config(&cfg(vec![], 1.0)).unwrap_err();
        assert!(matches!(err, JudgeBuildError::NoRubrics { .. }));
    }

    #[test]
    fn from_config_rejects_out_of_range_sampling_rate() {
        let err = Judge::from_config(&cfg(vec![("helpfulness", "...")], 1.5)).unwrap_err();
        assert!(matches!(err, JudgeBuildError::InvalidSamplingRate { .. }));
    }

    #[test]
    fn from_config_rejects_unknown_provider() {
        let mut c = cfg(vec![("helpfulness", "...")], 1.0);
        c.provider = "not-a-provider".into();
        let err = Judge::from_config(&c).unwrap_err();
        assert!(matches!(err, JudgeBuildError::UnknownProvider { .. }));
    }

    #[test]
    fn preamble_lists_criteria_alphabetically() {
        let judge = Judge::from_config(&cfg(
            vec![("tone", "be polite"), ("accuracy", "be factual")],
            1.0,
        ))
        .unwrap();
        let accuracy_at = judge.preamble.find("- accuracy:").unwrap();
        let tone_at = judge.preamble.find("- tone:").unwrap();
        assert!(accuracy_at < tone_at);
        assert_eq!(judge.criteria, vec!["accuracy", "tone"]);
    }

    #[test]
    fn sampling_rate_zero_never_samples() {
        let judge = Judge::from_config(&cfg(vec![("x", "...")], 0.0)).unwrap();
        for _ in 0..100 {
            assert!(!judge.should_sample());
        }
    }

    #[test]
    fn sampling_rate_one_always_samples() {
        let judge = Judge::from_config(&cfg(vec![("x", "...")], 1.0)).unwrap();
        for _ in 0..100 {
            assert!(judge.should_sample());
        }
    }

    #[test]
    fn parse_scores_accepts_raw_json() {
        let raw = r#"{"accuracy": {"score": 8, "reasoning": "mostly right"}}"#;
        let parsed = parse_scores(raw).unwrap();
        assert_eq!(parsed["accuracy"].score, 8.0);
        assert_eq!(parsed["accuracy"].reasoning, "mostly right");
    }

    #[test]
    fn parse_scores_strips_code_fences() {
        let raw = "```json\n{\"helpfulness\": {\"score\": 5, \"reasoning\": \"meh\"}}\n```";
        let parsed = parse_scores(raw).unwrap();
        assert_eq!(parsed["helpfulness"].score, 5.0);
    }

    #[test]
    fn parse_scores_errors_on_empty() {
        assert!(parse_scores("   ").is_err());
    }

    #[test]
    fn parse_scores_errors_on_non_json() {
        assert!(parse_scores("not json").is_err());
    }

    #[test]
    fn clamp_score_bounds_to_0_10() {
        assert_eq!(clamp_score(-3.0), 0.0);
        assert_eq!(clamp_score(12.5), 10.0);
        assert_eq!(clamp_score(7.2), 7.2);
        assert_eq!(clamp_score(f32::NAN), 0.0);
    }
}
