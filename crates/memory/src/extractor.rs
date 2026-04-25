use std::sync::Arc;

use coulisse_core::{OneShotPrompt, UserId};
use serde::Deserialize;

use crate::{ExtractorConfig, MemoryKind, Store};

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

/// Auto-extraction of durable user facts from each exchange.
///
/// Holds the parsed extractor config plus an `Arc<dyn OneShotPrompt>` used
/// to call out to whichever LLM does the extraction. The provider is
/// validated lazily on first use rather than at startup so memory does
/// not need to know which providers exist.
pub struct Extractor {
    completer: Arc<dyn OneShotPrompt>,
    dedup_threshold: f32,
    max_facts_per_turn: usize,
    model: String,
    provider: String,
}

impl Extractor {
    pub fn new(config: ExtractorConfig, completer: Arc<dyn OneShotPrompt>) -> Self {
        Self {
            completer,
            dedup_threshold: config.dedup_threshold,
            max_facts_per_turn: config.max_facts_per_turn,
            model: config.model,
            provider: config.provider,
        }
    }

    pub fn dedup_threshold(&self) -> f32 {
        self.dedup_threshold
    }

    pub fn max_facts_per_turn(&self) -> usize {
        self.max_facts_per_turn
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Spawn a background task that extracts durable facts from the last
    /// exchange and writes any novel ones into the user's memory. Never
    /// blocks the response; failures are logged and swallowed.
    pub fn spawn(
        self: &Arc<Self>,
        memory: Arc<Store>,
        user_id: UserId,
        user_message: String,
        assistant_message: String,
    ) {
        let extractor = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(err) = extractor
                .run(&memory, user_id, &user_message, &assistant_message)
                .await
            {
                tracing::warn!(user = %user_id.0, error = %err, "memory extraction failed");
            }
        });
    }

    async fn run(
        &self,
        memory: &Store,
        user_id: UserId,
        user_message: &str,
        assistant_message: &str,
    ) -> Result<(), String> {
        let user_text = format!(
            "User: {user_message}\n\nAssistant: {assistant_message}\n\nReturn the JSON array now."
        );
        let raw_text = self
            .completer
            .one_shot(&self.provider, &self.model, PREAMBLE, &user_text)
            .await
            .map_err(|e| format!("prompt: {e}"))?;

        let facts = parse_facts(&raw_text)?;
        let scope = memory.for_user(user_id);
        for fact in facts.into_iter().take(self.max_facts_per_turn) {
            if let Err(err) = scope
                .remember_if_novel(fact.kind, fact.content, self.dedup_threshold)
                .await
            {
                tracing::warn!(user = %user_id.0, error = %err, "failed to store extracted fact");
            }
        }
        Ok(())
    }
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
