//! Per-token cost lookup, sourced from a vendored `LiteLLM` snapshot.
//!
//! The pricing table is a snapshot of `LiteLLM`'s `model_prices_and_context_window.json`,
//! checked in under `data/model_prices.json` and refreshed via `just refresh-prices`.
//! Lookups are by `(provider, model)`; misses return `None` (caller logs once
//! and stores `null` in telemetry rather than failing the request).
//!
//! Pricing belongs here because it's intrinsic to a model — the same place
//! that already owns `ProviderKind` and the model-string callers pass to
//! `Provider::send`. cli builds one [`PricingTable`] at boot (parsing ~9k
//! JSON entries is measurably slow in debug builds, so it stays off the
//! request path) and computes cost at the moment it has the matching
//! `Usage`, so siblings (telemetry, limits) never depend on this module.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ProviderKind, Usage};

const RAW_PRICES: &str = include_str!("../data/model_prices.json");

/// Price of one token, in USD. Mirrors `LiteLLM`'s per-token fields
/// (`input_cost_per_token`, `cache_read_input_token_cost`, ...).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(transparent)]
struct CostPerToken(f64);

impl CostPerToken {
    /// Cost of `tokens` tokens at this rate, in USD.
    fn for_tokens(self, tokens: u64) -> f64 {
        // WHY: token counts in practice are well under 2^53 — f64
        // representation is exact for any value any provider would return.
        #[allow(clippy::cast_precision_loss)]
        let count = tokens as f64;
        count * self.0
    }
}

/// One model's per-token pricing (USD). All fields are optional because the
/// `LiteLLM` table is sparse — Groq entries have no cache pricing, older
/// `OpenAI` models have no `cache_read`, etc. Multiplying a missing field by a
/// token count yields zero, which is the right behavior.
#[derive(Clone, Debug, Default, Deserialize)]
struct ModelPricing {
    #[serde(default)]
    cache_creation_input_token_cost: Option<CostPerToken>,
    #[serde(default)]
    cache_read_input_token_cost: Option<CostPerToken>,
    #[serde(default)]
    input_cost_per_token: Option<CostPerToken>,
    #[serde(default)]
    litellm_provider: Option<String>,
    #[serde(default)]
    output_cost_per_token: Option<CostPerToken>,
}

impl ModelPricing {
    fn cost_of(&self, usage: &Usage) -> Cost {
        // WHY: LiteLLM's `input_cost_per_token` is the price for *uncached*
        // input tokens. Anthropic's `Usage::input_tokens` already excludes
        // cached reads and cache writes, so summing them here doesn't
        // double-count.
        let usd = self
            .input_cost_per_token
            .unwrap_or_default()
            .for_tokens(usage.input_tokens)
            + self
                .output_cost_per_token
                .unwrap_or_default()
                .for_tokens(usage.output_tokens)
            + self
                .cache_creation_input_token_cost
                .unwrap_or_default()
                .for_tokens(usage.cache_creation_input_tokens)
            + self
                .cache_read_input_token_cost
                .unwrap_or_default()
                .for_tokens(usage.cached_input_tokens);
        Cost::new(usd)
    }

    /// `LiteLLM` uses `litellm_provider` strings that mostly match our `ProviderKind`
    /// names but don't always — e.g. `vertex_ai-...` for some Gemini variants.
    /// We accept a prefix match on the provider's own name to avoid pulling the
    /// wrong row when two providers ship a same-named model.
    fn matches_provider(&self, provider: ProviderKind) -> bool {
        match self.litellm_provider.as_deref() {
            None => true,
            Some(name) => {
                name.starts_with(provider.as_str())
                    || provider.alternate_pricing_names().contains(name)
            }
        }
    }
}

impl ProviderKind {
    fn alternate_pricing_names(self) -> HashSet<&'static str> {
        match self {
            Self::Gemini => ["vertex_ai-language-models", "vertex_ai"]
                .into_iter()
                .collect(),
            _ => HashSet::new(),
        }
    }
}

/// Computed cost for one LLM call. Stored as USD (f64) — sub-cent precision
/// is fine for display and aggregation; we round at render time.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Cost {
    pub usd: f64,
}

impl Cost {
    #[must_use]
    pub fn new(usd: f64) -> Self {
        Self { usd }
    }
}

/// The parsed pricing table, keyed by `LiteLLM` model name. Build it once
/// with [`PricingTable::vendored`] and share it with whoever computes cost.
#[derive(Clone, Debug)]
pub struct PricingTable {
    by_model: HashMap<String, ModelPricing>,
}

impl PricingTable {
    /// Parse the vendored `LiteLLM` snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the vendored `model_prices.json` is not valid JSON.
    pub fn vendored() -> Result<Self, PricingParseError> {
        // WHY: the vendored file has one non-pricing entry (`sample_spec`)
        // used by LiteLLM as schema documentation; deserializing it as
        // `ModelPricing` fails because its fields are descriptive strings,
        // not numbers. Parse to `serde_json::Value` first and skip rows
        // that don't deserialize cleanly.
        let raw: serde_json::Value =
            serde_json::from_str(RAW_PRICES).map_err(|source| PricingParseError { source })?;
        let serde_json::Value::Object(map) = raw else {
            return Ok(Self {
                by_model: HashMap::default(),
            });
        };
        let by_model = map
            .into_iter()
            .filter_map(|(k, v)| {
                if k == "sample_spec" {
                    return None;
                }
                serde_json::from_value::<ModelPricing>(v)
                    .ok()
                    .map(|p| (k, p))
            })
            .collect();
        Ok(Self { by_model })
    }

    /// Compute cost from token usage. Returns `None` when the model isn't in
    /// the pricing table — caller decides whether to log, default to zero, or
    /// surface the gap.
    #[must_use]
    pub fn cost_for(&self, provider: ProviderKind, model: &str, usage: &Usage) -> Option<Cost> {
        self.lookup(provider, model).map(|p| p.cost_of(usage))
    }

    /// Look up a model's pricing entry. `LiteLLM` keys some models bare
    /// (`gpt-4o-mini`, `claude-sonnet-4-5-20250929`) and some prefixed
    /// (`groq/llama-3.3-70b-versatile`). Try the bare key first, then prefixed.
    fn lookup(&self, provider: ProviderKind, model: &str) -> Option<&ModelPricing> {
        if let Some(p) = self.by_model.get(model)
            && p.matches_provider(provider)
        {
            return Some(p);
        }
        let prefixed = format!("{}/{}", provider.as_str(), model);
        self.by_model.get(prefixed.as_str())
    }
}

#[derive(Debug, Error)]
#[error("vendored model_prices.json is not valid JSON: {source}")]
pub struct PricingParseError {
    #[source]
    source: serde_json::Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PricingTable {
        PricingTable::vendored().expect("vendored table parses")
    }

    #[test]
    fn known_anthropic_model_returns_nonzero_cost() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            total_tokens: 1_500,
            ..Default::default()
        };
        let cost = table()
            .cost_for(
                ProviderKind::Anthropic,
                "claude-sonnet-4-5-20250929",
                &usage,
            )
            .expect("known model");
        assert!(cost.usd > 0.0, "cost should be positive: {}", cost.usd);
    }

    #[test]
    fn unknown_model_returns_none() {
        let usage = Usage {
            input_tokens: 1,
            output_tokens: 1,
            total_tokens: 2,
            ..Default::default()
        };
        assert!(
            table()
                .cost_for(ProviderKind::Openai, "totally-made-up-model", &usage)
                .is_none()
        );
    }

    #[test]
    fn provider_prefix_lookup_finds_groq_models() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 1_000,
            total_tokens: 2_000,
            ..Default::default()
        };
        let cost = table()
            .cost_for(ProviderKind::Groq, "llama-3.3-70b-versatile", &usage)
            .expect("prefixed key");
        assert!(cost.usd > 0.0);
    }

    #[test]
    fn cache_tokens_priced_separately_from_input() {
        let with_cache = Usage {
            cache_creation_input_tokens: 1_000,
            cached_input_tokens: 1_000,
            input_tokens: 1_000,
            output_tokens: 0,
            total_tokens: 3_000,
        };
        let without_cache = Usage {
            input_tokens: 1_000,
            total_tokens: 1_000,
            ..Default::default()
        };
        let table = table();
        let with_c = table
            .cost_for(
                ProviderKind::Anthropic,
                "claude-sonnet-4-5-20250929",
                &with_cache,
            )
            .expect("known model");
        let without_c = table
            .cost_for(
                ProviderKind::Anthropic,
                "claude-sonnet-4-5-20250929",
                &without_cache,
            )
            .expect("known model");
        assert!(with_c.usd > without_c.usd);
    }
}
