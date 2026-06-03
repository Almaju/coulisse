# Memory

Coulisse remembers two things automatically:

1. **Conversation history** — every message in every turn, per user. Always on.
2. **User state** — durable facts and preferences extracted from those conversations and recalled into future prompts. Off by default; one line of YAML turns it on.

## Quick start

The simplest possible memory config is **no config at all** — omit the `memory:`
block and you get:

- Conversation history kept in SQLite at `.coulisse/coulisse-memory.db`.
- Long-term user state **off**.

To turn on long-term user state, that's the only line you write:

```yaml
memory:
  user_state: true
```

Now Coulisse will, after each turn:

- Ask a small "haiku-tier" model what's worth remembering about the user.
- Embed those facts and store them.
- On future requests, recall the most relevant ones and inject them into the prompt as a `Known about the user:` block.

You don't pick the embedder or the extraction model — Coulisse derives both automatically from your `providers:` block. (See [auto-derivation](#auto-derivation) below for the rules.)

## What gets injected into the prompt

When user state is on, every request to an agent gets a system message like:

```text
Known about the user:
- [fact] lives in Paris
- [preference] prefers WhatsApp-style short answers
```

…inserted *after* your agent's preamble and *before* the conversation history.

## Where data lives

There is nothing to configure. The database is always `.coulisse/coulisse-memory.db`
— the project state directory next to your `coulisse.yaml`, alongside the log,
PID, MCP secrets, and uploaded files. Created on first boot if missing.

For Docker, mount the `.coulisse/` directory on a volume so it survives container
restarts.

---

## Advanced

You usually don't need any of this. Skip unless you have a specific reason — defaults are picked to "just work" for the common case.

### Picking the extraction model explicitly

By default Coulisse picks the cheapest available model from your `providers:`. To pin one:

```yaml
memory:
  user_state:
    learn_from:
      provider: anthropic
      model: claude-haiku-4-5-20251001
```

### Picking the embedder explicitly

```yaml
memory:
  user_state:
    embed_with:
      provider: voyage
      model: voyage-3.5
      api_key: pa-...               # required for Voyage
```

Voyage is the only embedder that needs an explicit API key here — `openai` reuses the key from your top-level `providers.openai` entry.

### Recall and dedup tuning

```yaml
memory:
  user_state:
    recall_k: 5             # how many facts to recall per request
    dedup_threshold: 0.9    # cosine similarity above which a "new" fact is dropped
    max_facts_per_turn: 5   # cap on facts written per exchange
```

### Auto-derivation

When `user_state: true` (or when fields under `user_state:` are omitted):

- **Embedder.** If `openai` is in your `providers:`, Coulisse uses `text-embedding-3-small` and reuses the OpenAI key. Otherwise it falls back to the offline `hash` embedder (deterministic, no semantic understanding — fine for tests, never for production).
- **Extraction model.** Coulisse picks the first configured provider in this priority order — `anthropic` → `openai` → `gemini` → `groq` → `deepseek` → `cohere` — and uses its known cheap model (e.g. `claude-haiku-4-5-20251001`, `gpt-4o-mini`).

If `user_state: true` but you have no providers configured, Coulisse refuses to start with a clear error.

### Supported embedder models

- **`openai`**: `text-embedding-3-small` (1536 dims, default), `text-embedding-3-large` (3072 dims), `text-embedding-ada-002` (1536 dims).
- **`voyage`**: `voyage-3.5` (1024, default), `voyage-3-large` (1024), `voyage-3.5-lite` (1024), `voyage-code-3` (1024), `voyage-finance-2` (1024), `voyage-law-2` (1024), `voyage-code-2` (1536).
- **`hash`**: any positive `dims` (default 32). Offline only.

Unknown model names fail at startup with a clear error.

## Disabling user state

Either omit the `user_state:` field entirely or set it to `false`:

```yaml
memory:
  user_state: false
```

When disabled, Coulisse keeps conversation history but performs no extraction and no recall.

## Example configs

### Anthropic only — auto-everything

```yaml
providers:
  anthropic:
    api_key: sk-ant-...

memory:
  user_state: true
```

Auto-resolution: extraction uses `claude-haiku-4-5-20251001`, embeddings fall back to the offline `hash` embedder (because Voyage needs an explicit api_key).

### OpenAI end-to-end

```yaml
providers:
  openai:
    api_key: sk-...

memory:
  user_state: true
```

Auto-resolution: extraction uses `gpt-4o-mini`, embeddings use `text-embedding-3-small` with the OpenAI key.

### Anthropic completions + Voyage embeddings

```yaml
providers:
  anthropic:
    api_key: sk-ant-...

memory:
  user_state:
    embed_with:
      provider: voyage
      model: voyage-3.5
      api_key: pa-...
```

### Offline dev — no external calls

Omit the `memory:` block entirely (or set `user_state: false`): conversation
history is kept on disk under `.coulisse/`, with no extraction or embedding API
calls. Delete the database any time with `coulisse reset`.
