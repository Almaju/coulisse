use std::sync::Arc;

use config::ProviderKind;
use memory::{ExtractorConfig, MemoryKind, Store, UserId};
use prompter::{Message as PrompterMessage, Prompter, Role as PrompterRole};
use serde::Deserialize;

const PREAMBLE: &str = "You extract durable facts about a user from a single \
conversation exchange. You return ONLY a JSON array — no prose, no markdown, \
no explanations. Each array element is an object with two keys:\n\
- \"content\": the fact, phrased in the third person as \"user ...\" (e.g. \
\"user lives in Paris\", \"user prefers dark mode\").\n\
- \"kind\": either \"fact\" (objective, long-lived info) or \"preference\" \
(taste, opinion, setting).\n\n\
Return [] when nothing durable is stated. DO NOT extract:\n\
- Information about the assistant.\n\
- Transient task context (e.g. \"user wants a recipe\").\n\
- Information the user did not themselves volunteer.\n\
- Facts already obvious from the assistant's reply alone.";

/// Runtime extractor built from YAML. Validated at startup so the request
/// path only sees a known-good provider and model.
pub struct Extractor {
    pub dedup_threshold: f32,
    pub max_facts_per_turn: usize,
    pub model: String,
    pub provider: ProviderKind,
}

impl Extractor {
    /// Validate an ExtractorConfig against the set of configured providers.
    pub fn from_config(config: &ExtractorConfig) -> Result<Self, ExtractorBuildError> {
        let provider = ProviderKind::parse(&config.provider)
            .ok_or_else(|| ExtractorBuildError::UnknownProvider(config.provider.clone()))?;
        Ok(Self {
            dedup_threshold: config.dedup_threshold,
            max_facts_per_turn: config.max_facts_per_turn,
            model: config.model.clone(),
            provider,
        })
    }
}

/// Spawn a background task that extracts durable facts from the last
/// exchange and writes any novel ones into the user's memory. Never blocks
/// the response; failures are logged and swallowed.
pub fn spawn_extract<P: Prompter + 'static>(
    extractor: Arc<Extractor>,
    memory: Arc<Store>,
    prompter: Arc<P>,
    user_id: UserId,
    user_message: String,
    assistant_message: String,
) {
    tokio::spawn(async move {
        if let Err(err) = run_extract(
            &extractor,
            &memory,
            prompter.as_ref(),
            user_id,
            &user_message,
            &assistant_message,
        )
        .await
        {
            tracing::warn!(user = %user_id.0, error = %err, "memory extraction failed");
        }
    });
}

async fn run_extract<P: Prompter>(
    extractor: &Extractor,
    memory: &Store,
    prompter: &P,
    user_id: UserId,
    user_message: &str,
    assistant_message: &str,
) -> Result<(), ExtractRunError> {
    let turn = PrompterMessage {
        content: format!(
            "User: {user_message}\n\nAssistant: {assistant_message}\n\nReturn the JSON array now."
        ),
        role: PrompterRole::User,
    };
    let completion = prompter
        .prompt_with(extractor.provider, &extractor.model, PREAMBLE, vec![turn])
        .await
        .map_err(ExtractRunError::Prompt)?;

    let facts = parse_facts(&completion.text).map_err(ExtractRunError::Parse)?;
    let scope = memory.for_user(user_id);
    for fact in facts.into_iter().take(extractor.max_facts_per_turn) {
        if let Err(err) = scope
            .remember_if_novel(fact.kind, fact.content, extractor.dedup_threshold)
            .await
        {
            tracing::warn!(user = %user_id.0, error = %err, "failed to store extracted fact");
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RawFact {
    content: String,
    kind: String,
}

struct ParsedFact {
    content: String,
    kind: MemoryKind,
}

fn parse_facts(text: &str) -> Result<Vec<ParsedFact>, String> {
    let trimmed = strip_code_fence(text.trim()).trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let raw: Vec<RawFact> = serde_json::from_str(trimmed)
        .map_err(|e| format!("extractor returned non-JSON output ({e}): {trimmed:?}"))?;
    let mut out = Vec::with_capacity(raw.len());
    for r in raw {
        let kind = match r.kind.as_str() {
            "fact" => MemoryKind::Fact,
            "preference" => MemoryKind::Preference,
            other => {
                tracing::debug!(kind = other, "skipping extracted entry with unknown kind");
                continue;
            }
        };
        let content = r.content.trim().to_string();
        if content.is_empty() {
            continue;
        }
        out.push(ParsedFact { content, kind });
    }
    Ok(out)
}

/// Strip a single ```json ... ``` or ``` ... ``` fence if present. Models
/// sometimes wrap JSON in markdown despite instructions.
fn strip_code_fence(s: &str) -> &str {
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    let s = s.trim_start();
    s.strip_suffix("```").unwrap_or(s)
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractorBuildError {
    #[error(
        "memory.extractor.provider '{0}' is not a supported provider (anthropic, cohere, deepseek, gemini, groq, openai)"
    )]
    UnknownProvider(String),
}

#[derive(Debug, thiserror::Error)]
enum ExtractRunError {
    #[error("parse: {0}")]
    Parse(String),
    #[error("prompt: {0}")]
    Prompt(prompter::PrompterError),
}
