# Per-user memory

Every request that carries a user identifier gets an isolated, persistent memory scope. Coulisse tracks two kinds of memory:

- **Conversation history** — the running transcript of messages the user has exchanged.
- **Long-term memories** — durable facts and preferences, embedded for semantic recall.

You don't need to manage this — it happens automatically on every request. When auto-extraction is on, Coulisse also decides *what* is worth remembering.

## What happens on each request

1. Coulisse identifies the user from `safety_identifier` (or `user`).
2. It pulls the user's recent messages, fitting as many as possible into the context budget.
3. It runs a semantic recall against the user's long-term memories, picking the top matches.
4. It builds the final prompt: agent preamble → recalled memories → recent history → new message.
5. The model's reply is sent back and saved to the user's transcript.
6. If an extractor is configured, a background task asks a cheap model *"any durable facts to remember from this exchange?"* and stores novel ones.

Step 6 does not block the HTTP response — the user gets their answer first; memory grows in the background.

## Isolation guarantees

User isolation is enforced by the API: `Store::for_user(id)` returns a handle scoped to a single user, and every SQL query bound through it filters on that user id. There is no code path that mixes data across users.

## The context budget

| Knob                    | Default     | Meaning |
|-------------------------|-------------|---------|
| `context_budget`        | 8,000 tokens | Total window size for messages + memories. |
| `memory_budget_fraction`| 0.1 (10%)   | Share of the budget reserved for recalled long-term memories. |
| `recall_k`              | 5            | How many long-term memories to recall per request. |

The remaining 90% goes to recent message history, newest first. If the history doesn't fit, older messages are dropped.

## Embedders

Long-term memories are embedded as vectors. On each request, Coulisse embeds the incoming message and retrieves the top-k most similar memories by cosine similarity. That's how context from a conversation two weeks ago can surface when it becomes relevant again.

| Provider | Supported models | Notes |
|----------|------------------|-------|
| `openai` | `text-embedding-3-small`, `text-embedding-3-large`, `text-embedding-ada-002` | Default pairing for OpenAI-first setups. |
| `voyage` | `voyage-3.5`, `voyage-3-large`, `voyage-3.5-lite`, `voyage-code-3`, `voyage-finance-2`, `voyage-law-2`, `voyage-code-2` | Anthropic officially recommends Voyage for embeddings. |
| `hash`   | n/a              | Deterministic bag-of-words, **offline only**. No semantic understanding — use only for tests and air-gapped development. |

Startup logs the chosen embedder. For `hash` the log line carries an explicit "OFFLINE — no semantic understanding" tag so nobody deploys it by accident.

## Auto-extraction ("remember what matters")

When you set `memory.extractor` in YAML, every completed exchange fires a background task that:

1. Sends the last user-turn + assistant-turn to a cheap model with a focused prompt: *"list any durable facts or preferences about the user; return `[]` if nothing worth keeping."*
2. Parses the JSON response.
3. For each extracted fact, calls `remember_if_novel` — which embeds the fact and skips it if cosine similarity against an existing memory exceeds `dedup_threshold` (default 0.9).

Failures (bad JSON, timeout, provider error) are logged at `warn` and swallowed — the user already got their response. Extraction is best-effort.

To disable, omit the `memory.extractor` block entirely. Memories will still be recalled and can be populated through other code paths, but nothing writes to them automatically.

## What gets stored where

| Data                          | Scope       | Lifetime |
|-------------------------------|-------------|----------|
| Conversation messages         | Per user    | SQLite (`messages` table) |
| Long-term memories + vectors  | Per user    | SQLite (`memories` table, BLOB embeddings) |
| User identifier → internal ID | Shared      | Derived deterministically — no storage needed |

Each memory row carries the id of the embedder that produced it. If you swap the embedder, old vectors become ineligible for recall (they'd be scored in the wrong space). They stay in the database but are silently ignored until you re-embed them.

## Storage location

Defaults to `./coulisse-memory.db`. Override with:

```yaml
memory:
  backend:
    kind: sqlite
    path: /var/lib/coulisse/memory.db
```

For tests or one-shot demos, use `kind: in_memory` — everything evaporates on shutdown.

### Docker

The bundled Dockerfile declares a `VOLUME /var/lib/coulisse` so data survives container restarts. Mount a named volume or a host directory there:

```bash
docker run \
  -v coulisse-data:/var/lib/coulisse \
  -v $(pwd)/coulisse.yaml:/etc/coulisse/coulisse.yaml:ro \
  -p 8421:8421 \
  coulisse
```

The container runs as a non-root `coulisse` user and expects the database path inside the volume, e.g. `/var/lib/coulisse/memory.db`.

See [memory configuration](../configuration/memory.md) for the full YAML schema.
