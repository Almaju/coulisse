# Token cost tracking

Coulisse converts each chat completion's token usage into a USD cost using a vendored snapshot of [LiteLLM's model pricing table](https://github.com/BerriAI/litellm/blob/main/litellm/model_prices_and_context_window_backup.json). The cost lands in the per-turn `llm_call` event alongside the raw token counts, so the studio UI shows it next to every model call.

There's nothing to enable. As long as a turn produces token usage and the model is in the table, you'll see a `$0.0042`-style badge on the corresponding `llm_call` row in the per-turn event tree.

## How it's computed

For each completion Coulisse looks up the configured `(provider, model)` pair in the vendored table and multiplies:

- `input_tokens × input_cost_per_token`
- `output_tokens × output_cost_per_token`
- `cache_creation_input_tokens × cache_creation_input_token_cost` (Anthropic prompt-cache writes)
- `cached_input_tokens × cache_read_input_token_cost` (Anthropic prompt-cache reads)

Missing fields in the upstream table are treated as zero — fine for providers like Groq that don't price cache tokens. Models that don't appear in the table at all yield a `null` cost: the request still succeeds, the `llm_call` event still records the token usage, and the studio simply omits the cost badge.

## Refreshing the pricing table

The snapshot lives at `crates/providers/data/model_prices.json` and is checked into git. New models are added upstream regularly; refresh the snapshot with:

```bash
just refresh-prices
```

This downloads the latest version from LiteLLM's main branch and overwrites the local file. The diff lands in git like any other change so you can review what moved before committing.

There's no live fetching at runtime: cost lookup only ever reads from the vendored snapshot. That keeps the request path free of network dependencies and makes pricing updates an explicit, reviewable action.

## What's not (yet) covered

- **EUR or other currencies.** Cost is stored and displayed in USD only. If there's demand for a configurable display currency (`telemetry.display_currency: { code: EUR, usd_rate: 0.92 }`-style), it can be added without changing the on-disk format.
- **Cost-based rate limiting.** [Rate limits](./rate-limiting.md) currently work on token counts. Cost is recorded but not yet enforced; a future `usd_per_day:` knob would consume the same data.
- **Per-tool / per-MCP cost.** Tool calls have their own `tool_call` events but don't carry a cost themselves. Costs are charged to the parent `llm_call` event, which is the only place tokens are spent.
- **Custom or unlisted models.** Self-hosted models or models that LiteLLM hasn't added yet won't have a price. There's no YAML override path today; if you need one, open an issue describing the use case.
