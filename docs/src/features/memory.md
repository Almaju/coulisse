# Per-user memory

Every request gets an isolated, persistent memory scope based on its user identity. In `users: per-request` mode, that identity comes from `safety_identifier` (or the deprecated `user` field) on each request; in the default `users: shared` mode, every request shares one hardcoded identity (and one memory bucket). See [User identification](../configuration/user-id.md). Coulisse tracks two kinds of memory:

- **Conversation history** — the running transcript of messages the user has exchanged. Always on.
- **Long-term user state** — durable facts and preferences, embedded for semantic recall. Off by default; opt in with `user_state: true`.

You don't manage either of these by hand — both are wired into every request automatically. When `user_state` is on, Coulisse also decides *what* is worth remembering after each turn.

## What happens on each request

1. Coulisse identifies the user — from `safety_identifier` / `user` in `per-request` mode, or from the shared identity in `shared` mode.
2. It pulls the user's recent messages, fitting as many as possible into the context window.
3. If long-term user state is on, it runs a semantic recall against the user's stored facts and picks the top matches.
4. It builds the final prompt: agent preamble → recalled facts (if any) → recent history → new message.
5. The model's reply is sent back and saved to the user's transcript.
6. If `user_state` is on, a background task asks a cheap model *"any durable facts to remember from this exchange?"* and stores novel ones.

Step 6 does not block the HTTP response — the user gets their answer first; long-term memory grows in the background.

## Isolation guarantees

User isolation is enforced by the API: `Store::for_user(id)` returns a handle scoped to a single user, and every SQL query bound through it filters on that user id. There is no code path that mixes data across users.

## How long-term recall works

When `user_state: true`, Coulisse embeds each stored fact as a vector at write time. On every request, it embeds the incoming user message and retrieves the top-k most similar facts by cosine similarity. That's how context from a conversation two weeks ago can surface when it becomes relevant again.

The recalled facts are formatted as a system block titled `Known about the user:` and injected into the prompt before the conversation history.

## Auto-extraction ("remember what matters")

When `user_state: true`, every completed exchange fires a background task that:

1. Sends the last user-turn + assistant-turn to a cheap model with a focused prompt: *"list any durable facts or preferences about the user; return `[]` if nothing worth keeping."*
2. Parses the JSON response.
3. For each extracted fact, calls `remember_if_novel` — which embeds the fact and skips it if cosine similarity against an existing memory exceeds `dedup_threshold` (default 0.9).

Failures (bad JSON, timeout, provider error) are logged at `warn` and swallowed — the user already got their response. Extraction is best-effort.

To disable, omit the `user_state:` field or set it to `false`. Conversation history is unaffected either way.

## Embedders

| Provider | Supported models | Notes |
|----------|------------------|-------|
| `openai` | `text-embedding-3-small`, `text-embedding-3-large`, `text-embedding-ada-002` | Default pairing for OpenAI-first setups. |
| `voyage` | `voyage-3.5`, `voyage-3-large`, `voyage-3.5-lite`, `voyage-code-3`, `voyage-finance-2`, `voyage-law-2`, `voyage-code-2` | Anthropic officially recommends Voyage for embeddings. Requires an explicit `api_key`. |
| `hash`   | n/a              | Deterministic bag-of-words, **offline only**. No semantic understanding — use only for tests and air-gapped development. |

When `user_state: true` and you don't pin an embedder explicitly, Coulisse picks one for you (see [auto-derivation](../configuration/memory.md#auto-derivation)). Startup logs the chosen embedder.

## What gets stored where

| Data                          | Scope       | Lifetime |
|-------------------------------|-------------|----------|
| Conversation messages         | Per user    | SQLite (`messages` table) |
| Long-term memories + vectors  | Per user    | SQLite (`memories` table, BLOB embeddings) |
| Tool invocations              | Per user    | SQLite (`tool_calls` table, linked to `messages.id`) |
| Judge scores                  | Per user    | SQLite (`scores` table, linked to `messages.id`) |
| User identifier → internal ID | Shared      | Derived deterministically — no storage needed |

Each memory row carries the id of the embedder that produced it. If you swap the embedder, old vectors become ineligible for recall (they'd be scored in the wrong space). They stay in the database but are silently ignored until you re-embed them.

## Storage location

Defaults to `./coulisse-memory.db`. Override with:

```yaml
memory:
  storage: /var/lib/coulisse/memory.db
```

For tests or one-shot demos, use `storage: ":memory:"` — everything evaporates on shutdown.

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
