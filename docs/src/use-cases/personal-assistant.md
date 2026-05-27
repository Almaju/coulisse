# Personal assistant with memory

**What you get:** a chat assistant that remembers facts about you — preferences, context, past conversations — and surfaces them automatically in future sessions.

## The config

```yaml
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
  voyage:
    api_key: ${VOYAGE_API_KEY}

memory:
  user_state: true
  embedder:
    provider: voyage
    model: voyage-3.5-lite

agents:
  - name: assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: |
      You are a personal assistant. Be concise and direct.
      Use what you know about the user to personalise replies.
```

## What happens

1. You send a message — e.g. *"I'm switching to a plant-based diet."*
2. Coulisse delivers the reply immediately.
3. In the background, a cheap model reads the exchange and extracts the durable fact: *"User follows a plant-based diet."* It's stored in SQLite with a vector embedding.
4. Next time you ask *"what's a quick lunch idea?"*, that fact is recalled by semantic similarity and injected into the prompt before your question. The assistant suggests plant-based options without you having to repeat yourself.

You see none of this machinery. From the outside it's just a chat endpoint that gets progressively smarter about you.

## Try it

```bash
curl http://localhost:8421/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "assistant",
    "messages": [{"role": "user", "content": "I prefer short, direct answers."}]
  }'
```

Then in a later session:

```bash
curl http://localhost:8421/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "assistant",
    "messages": [{"role": "user", "content": "Explain quantum entanglement."}]
  }'
```

The assistant will be brief, because it remembered.

## Notes

- `user_state: true` turns on both auto-extraction and semantic recall.
- No Voyage account? Use `provider: openai` and `model: text-embedding-3-small`. Or set `provider: hash` for offline development — no semantic understanding, but the rest of the pipeline works.
- Multiple users: see [User identification](../configuration/user-id.md) to partition memory by user.

**Next:** [Multi-model app](./multi-model.md)
