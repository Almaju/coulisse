use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use coulisse_core::OneShotPrompt;
use serde::Deserialize;

use crate::JudgeConfig;
use crate::store::Judges;
use crate::types::{Score, ScoredExchange};

/// Runtime judge built from YAML and validated at startup. Holds the
/// prebuilt preamble so the hot path does zero string construction before
/// hitting the model.
#[derive(Debug)]
pub struct Judge {
    pub criteria: Vec<String>,
    pub model: String,
    pub name: String,
    pub preamble: String,
    /// Provider name as written in YAML (e.g. "openai"). Cli validates the
    /// string against the providers map at config load; runtime errors
    /// surface from `OneShotPrompt::one_shot` if it ever drifts.
    pub provider: String,
    pub sampling_rate: f32,
}

impl Judge {
    /// Validate a `JudgeConfig` and produce a ready-to-run `Judge`. Provider
    /// name is checked by cli's cross-feature config validation, so this
    /// only handles judge-local invariants.
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
        let criteria: Vec<String> = config.rubrics.keys().cloned().collect();
        Ok(Self {
            preamble: build_preamble(&config.rubrics),
            criteria,
            model: config.model.clone(),
            name: config.name.clone(),
            provider: config.provider.clone(),
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
pub fn spawn_score<C: OneShotPrompt + 'static>(
    judges: Vec<Arc<Judge>>,
    store: Arc<Judges>,
    completer: Arc<C>,
    exchange: ScoredExchange,
) {
    if judges.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for judge in judges {
            if !judge.should_sample() {
                continue;
            }
            if let Err(err) = run_score(&judge, &store, completer.as_ref(), &exchange).await {
                tracing::warn!(
                    user = %exchange.user_id.0,
                    judge = %judge.name,
                    error = %err,
                    "judge scoring failed",
                );
            }
        }
    });
}

async fn run_score(
    judge: &Judge,
    store: &Judges,
    completer: &dyn OneShotPrompt,
    exchange: &ScoredExchange,
) -> Result<(), JudgeRunError> {
    let user_text = format!(
        "User message:\n{}\n\nAssistant reply:\n{}\n\nReturn the JSON object now.",
        exchange.user_message, exchange.assistant_message,
    );
    let raw_text = completer
        .one_shot(&judge.provider, &judge.model, &judge.preamble, &user_text)
        .await
        .map_err(|e| JudgeRunError::Prompt(e.to_string()))?;
    let raw = parse_scores(&raw_text).map_err(JudgeRunError::Parse)?;
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
            exchange.user_id,
            exchange.message_id,
            exchange.agent_name.clone(),
            judge.name.clone(),
            judge.model.clone(),
            criterion.clone(),
            clamp_score(raw_score.score),
            raw_score.reasoning.clone(),
        );
        if let Err(err) = store.append_score(score).await {
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
    use std::fmt::Write as _;
    let mut out = String::from(
        "You are an evaluation judge. Score the assistant's reply against each \
         criterion below on an integer scale from 0 (worst) to 10 (best), with a \
         concise one-sentence reasoning.\n\nCriteria:\n",
    );
    for (name, description) in rubrics {
        let _ = writeln!(out, "- {name}: {description}");
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
}

#[derive(Debug, thiserror::Error)]
enum JudgeRunError {
    #[error("parse: {0}")]
    Parse(String),
    #[error("prompt: {0}")]
    Prompt(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(rubrics: &[(&str, &str)], rate: f32, provider: &str) -> JudgeConfig {
        JudgeConfig {
            model: "gpt-mini".into(),
            name: "quality".into(),
            provider: provider.into(),
            rubrics: rubrics
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            sampling_rate: rate,
        }
    }

    #[test]
    fn from_config_rejects_empty_rubrics() {
        let cfg = config(&[], 1.0, "openai");
        assert!(matches!(
            Judge::from_config(&cfg),
            Err(JudgeBuildError::NoRubrics { .. })
        ));
    }

    #[test]
    fn from_config_rejects_out_of_range_sampling_rate() {
        let cfg = config(&[("a", "b")], 1.5, "openai");
        assert!(matches!(
            Judge::from_config(&cfg),
            Err(JudgeBuildError::InvalidSamplingRate { value, .. })
                if (value - 1.5).abs() < f32::EPSILON,
        ));
    }

    #[test]
    fn sampling_rate_one_always_samples() {
        let cfg = config(&[("a", "b")], 1.0, "openai");
        let judge = Judge::from_config(&cfg).unwrap();
        for _ in 0..50 {
            assert!(judge.should_sample());
        }
    }

    #[test]
    fn sampling_rate_zero_never_samples() {
        let cfg = config(&[("a", "b")], 0.0, "openai");
        let judge = Judge::from_config(&cfg).unwrap();
        for _ in 0..50 {
            assert!(!judge.should_sample());
        }
    }

    #[test]
    fn preamble_lists_criteria_alphabetically() {
        let cfg = config(&[("zebra", "z desc"), ("alpha", "a desc")], 1.0, "openai");
        let judge = Judge::from_config(&cfg).unwrap();
        let alpha_pos = judge.preamble.find("alpha").unwrap();
        let zebra_pos = judge.preamble.find("zebra").unwrap();
        assert!(alpha_pos < zebra_pos);
    }

    #[test]
    fn parse_scores_accepts_raw_json() {
        let text = r#"{"clarity": {"score": 8, "reasoning": "clear"}}"#;
        let parsed = parse_scores(text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed["clarity"].score as i32, 8);
        assert_eq!(parsed["clarity"].reasoning, "clear");
    }

    #[test]
    fn parse_scores_strips_code_fences() {
        let text = "```json\n{\"clarity\": {\"score\": 7, \"reasoning\": \"ok\"}}\n```";
        let parsed = parse_scores(text).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_scores_errors_on_empty() {
        assert!(parse_scores("").is_err());
        assert!(parse_scores("   ").is_err());
    }

    #[test]
    fn parse_scores_errors_on_non_json() {
        assert!(parse_scores("not json").is_err());
    }

    #[test]
    fn clamp_score_bounds_to_0_10() {
        assert!((clamp_score(-3.0) - 0.0).abs() < f32::EPSILON);
        assert!((clamp_score(15.0) - 10.0).abs() < f32::EPSILON);
        assert!((clamp_score(5.5) - 5.5).abs() < f32::EPSILON);
        assert!((clamp_score(f32::NAN) - 0.0).abs() < f32::EPSILON);
    }
}
