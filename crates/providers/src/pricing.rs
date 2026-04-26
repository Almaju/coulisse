//! Per-token cost lookup, sourced from a vendored LiteLLM snapshot.
//!
//! The pricing table is a snapshot of LiteLLM's `model_prices_and_context_window.json`,
//! checked in under `data/model_prices.json` and refreshed via `just refresh-prices`.
//! Lookups are by `(provider, model)`; misses return `None` (caller logs once
//! and stores `null` in telemetry rather than failing the request).
//!
//! Pricing belongs here because it's intrinsic to a model — the same place
//! that already owns `ProviderKind` and the model-string callers pass to
//! `Provider::send`. cli computes cost at the moment it has the matching
//! `Usage`, so siblings (telemetry, limits) never depend on this module.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::{ProviderKind, Usage};

const RAW_PRICES: &str = include_str!("../data/model_prices.json");

/// One model's per-token pricing (USD). All fields are optional because the
/// LiteLLM table is sparse — Groq entries have no cache pricing, older
/// OpenAI models have no cache_read, etc. Multiplying a missing field by a
/// token count yields zero, which is the right behavior.
#[derive(Clone, Debug, Default, Deserialize)]
struct ModelPricing {
    #[serde(default)]
    cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    cache_read_input_token_cost: Option<f64>,
    #[serde(default)]
    input_cost_per_token: Option<f64>,
    #[serde(default)]
    litellm_provider: Option<String>,
    #[serde(default)]
    output_cost_per_token: Option<f64>,
}

/// Computed cost for one LLM call. Stored as USD (f64) — sub-cent precision
/// is fine for display and aggregation; we round at render time.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Cost {
    pub usd: f64,
}

impl Cost {
    pub fn new(usd: f64) -> Self {
        Self { usd }
    }
}

/// Compute cost from token usage. Returns `None` when the model isn't in
/// the pricing table — caller decides whether to log, default to zero, or
/// surface the gap.
pub fn cost_for(provider: ProviderKind, model: &str, usage: &Usage) -> Option<Cost> {
    let pricing = lookup(provider, model)?;
    let cache_create = pricing.cache_creation_input_token_cost.unwrap_or(0.0);
    let cache_read = pricing.cache_read_input_token_cost.unwrap_or(0.0);
    let input = pricing.input_cost_per_token.unwrap_or(0.0);
    let output = pricing.output_cost_per_token.unwrap_or(0.0);

    // LiteLLM's `input_cost_per_token` is the price for *uncached* input
    // tokens. Anthropic's `Usage::input_tokens` already excludes cached
    // reads and cache writes, so summing them here doesn't double-count.
    let usd = (usage.input_tokens as f64) * input
        + (usage.output_tokens as f64) * output
        + (usage.cache_creation_input_tokens as f64) * cache_create
        + (usage.cached_input_tokens as f64) * cache_read;
    Some(Cost::new(usd))
}

/// Look up a model's pricing entry. LiteLLM keys some models bare
/// (`gpt-4o-mini`, `claude-sonnet-4-5-20250929`) and some prefixed
/// (`groq/llama-3.3-70b-versatile`). Try the bare key first, then prefixed.
fn lookup(provider: ProviderKind, model: &str) -> Option<&'static ModelPricing> {
    let table = table();
    if let Some(p) = table.get(model)
        && pricing_matches_provider(p, provider)
    {
        return Some(p);
    }
    let prefixed = format!("{}/{}", provider.as_str(), model);
    table.get(prefixed.as_str())
}

/// LiteLLM uses `litellm_provider` strings that mostly match our `ProviderKind`
/// names but don't always — e.g. `vertex_ai-...` for some Gemini variants.
/// We accept a prefix match on the provider's own name to avoid pulling the
/// wrong row when two providers ship a same-named model.
fn pricing_matches_provider(p: &ModelPricing, provider: ProviderKind) -> bool {
    match p.litellm_provider.as_deref() {
        None => true,
        Some(name) => {
            name.starts_with(provider.as_str()) || alternate_provider_names(provider).contains(name)
        }
    }
}

fn alternate_provider_names(provider: ProviderKind) -> HashSet<&'static str> {
    match provider {
        ProviderKind::Gemini => ["vertex_ai-language-models", "vertex_ai"]
            .into_iter()
            .collect(),
        _ => HashSet::new(),
    }
}

/// Force-load the vendored pricing table. Cheap on hit (just touches
/// the `OnceLock`), expensive on miss (~9k JSON entries to deserialize
/// — measurably slow in debug builds). Cli calls this during boot so
/// the first chat completion doesn't pay for it on the request path.
pub fn warm() {
    let _ = table();
}

fn table() -> &'static std::collections::HashMap<String, ModelPricing> {
    static TABLE: OnceLock<std::collections::HashMap<String, ModelPricing>> = OnceLock::new();
    TABLE.get_or_init(|| {
        // The vendored file has one non-pricing entry (`sample_spec`) used
        // by LiteLLM as schema documentation; deserializing it as
        // `ModelPricing` would fail because its fields are descriptive
        // strings, not numbers. Parse to `serde_json::Value` first and
        // skip rows that don't deserialize cleanly.
        let raw: serde_json::Value =
            serde_json::from_str(RAW_PRICES).expect("vendored model_prices.json is valid JSON");
        let serde_json::Value::Object(map) = raw else {
            return Default::default();
        };
        map.into_iter()
            .filter_map(|(k, v)| {
                if k == "sample_spec" {
                    return None;
                }
                serde_json::from_value::<ModelPricing>(v)
                    .ok()
                    .map(|p| (k, p))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_anthropic_model_returns_nonzero_cost() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            total_tokens: 1_500,
            ..Default::default()
        };
        let cost = cost_for(
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
        assert!(cost_for(ProviderKind::Openai, "totally-made-up-model", &usage).is_none());
    }

    #[test]
    fn provider_prefix_lookup_finds_groq_models() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 1_000,
            total_tokens: 2_000,
            ..Default::default()
        };
        let cost =
            cost_for(ProviderKind::Groq, "llama-3.3-70b-versatile", &usage).expect("prefixed key");
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
        let with_c = cost_for(
            ProviderKind::Anthropic,
            "claude-sonnet-4-5-20250929",
            &with_cache,
        )
        .expect("known model");
        let without_c = cost_for(
            ProviderKind::Anthropic,
            "claude-sonnet-4-5-20250929",
            &without_cache,
        )
        .expect("known model");
        assert!(with_c.usd > without_c.usd);
    }
}
