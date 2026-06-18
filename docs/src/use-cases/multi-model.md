# Multi-model app

**What you get:** multiple agents behind a single OpenAI-compatible endpoint. Your application picks which one to call by name — no extra infra, no routing logic in your code.

## The config

```yaml
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
  openai:
    api_key: ${OPENAI_API_KEY}

agents:
  - name: triage
    provider: anthropic
    model: claude-haiku-4-5-20251001
    preamble: |
      Classify the user's intent. Reply with exactly one word:
      SUPPORT, SALES, or OTHER.

  - name: support
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: |
      You are a customer support agent. Be empathetic, clear, and solution-focused.

  - name: sales
    provider: openai
    model: gpt-4o
    preamble: |
      You are a sales assistant. Help users understand product options
      and guide them toward the right plan.
```

## What happens

Your application calls `triage` first, reads the single-word reply, then routes to the right agent:

```python
import openai

client = openai.OpenAI(base_url="http://localhost:8421/v1", api_key="unused")

def handle(user_message: str) -> str:
    # fast, cheap classification
    intent = client.chat.completions.create(
        model="triage",
        messages=[{"role": "user", "content": user_message}],
    ).choices[0].message.content.strip()

    # route to the right specialist
    agent = {"SUPPORT": "support", "SALES": "sales"}.get(intent, "support")
    return client.chat.completions.create(
        model=agent,
        messages=[{"role": "user", "content": user_message}],
    ).choices[0].message.content
```

No Coulisse-specific SDK. No custom headers. If you later add a fourth agent, you add it to `coulisse.yaml` and adjust the routing map — nothing else changes.

## Discover agents at runtime

```bash
curl http://localhost:8421/v1/models
```

Returns all declared agents in OpenAI's standard model-list format. Useful for populating a model picker in a UI.

## Notes

- Each agent has its own provider, model, and preamble. Mixing Anthropic and OpenAI in the same file is fine.
- Agents share nothing by default. Memory is per-user but the same user talking to `support` and then `sales` accumulates memory in the same bucket — they can recall each other's context.
- For agents calling each other *within* a single turn, see [Orchestrator + specialists](./orchestrator-specialists.md).

**Next:** [Slack / webhook bot](./webhook-bot.md)
