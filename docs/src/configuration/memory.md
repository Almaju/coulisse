# Memory

The `memory:` block in `coulisse.yaml` controls where data is stored, which embedder turns text into vectors, and whether auto-extraction runs after each turn. Every field has a sensible default — omit the block entirely and Coulisse falls back to an on-disk SQLite file and the offline `hash` embedder.

## Shape

```yaml
memory:
  backend:
    kind: sqlite                   # 'sqlite' (default) or 'in_memory'
    path: ./coulisse-memory.db     # sqlite only
  embedder:
    provider: openai               # 'openai', 'voyage', or 'hash'
    model: text-embedding-3-small  # required for openai/voyage
    # api_key: <override>          # optional — falls back to providers.openai.api_key
  extractor:                       # omit to disable auto-extraction
    provider: anthropic            # one of providers.* keys
    model: claude-haiku-4-5-20251001
    dedup_threshold: 0.9           # optional
    max_facts_per_turn: 5          # optional
  context_budget: 8000             # optional
  memory_budget_fraction: 0.1      # optional
  recall_k: 5                      # optional
```

## `memory.backend`

| Field       | Type   | Required | Notes |
|-------------|--------|----------|-------|
| `kind`      | enum   | yes      | `sqlite` or `in_memory`. |
| `path`      | string | no       | Filesystem path for `sqlite`. Created if missing. Default `./coulisse-memory.db`. |

`in_memory` is a SQLite database that lives only for the process lifetime — use it for tests or throw-away demos. `sqlite` is the production default; for Docker, point `path` at a volume-mounted location (e.g. `/var/lib/coulisse/memory.db`).

## `memory.embedder`

| Field       | Type   | Required | Notes |
|-------------|--------|----------|-------|
| `provider`  | enum   | yes      | `openai`, `voyage`, or `hash`. |
| `model`     | string | depends  | Required for `openai` and `voyage`. Ignored for `hash`. |
| `api_key`   | string | no       | Falls back to `providers.<provider>.api_key` when unset. |
| `dims`      | int    | no       | Hash only. Default 32. |

### Supported models

- **`openai`**: `text-embedding-3-small` (1536 dims, default), `text-embedding-3-large` (3072 dims), `text-embedding-ada-002` (1536 dims).
- **`voyage`**: `voyage-3.5` (1024, default), `voyage-3-large` (1024), `voyage-3.5-lite` (1024), `voyage-code-3` (1024), `voyage-finance-2` (1024), `voyage-law-2` (1024), `voyage-code-2` (1536).

Unknown model names fail at startup with a clear error.

### Which to pick

- Using **Anthropic** for completions? Anthropic has no embedding API — use **Voyage** (their official recommendation).
- Using **OpenAI**? Stay on OpenAI for consistency.
- **Offline / air-gapped**? Use `hash` — it has no semantic understanding but is fast and deterministic.

## `memory.extractor`

Omit this block to disable auto-extraction. When present:

| Field                | Type   | Required | Notes |
|----------------------|--------|----------|-------|
| `provider`           | string | yes      | Must match a key under top-level `providers:`. |
| `model`              | string | yes      | Upstream model identifier. Prefer the cheapest usable model. |
| `dedup_threshold`    | float  | no       | Cosine similarity above which an extracted fact is considered a duplicate. Default 0.9. |
| `max_facts_per_turn` | int    | no       | Cap on facts written per exchange. Default 5. |

The extractor runs as a background task after each successful completion — it never blocks the HTTP response. Failures are logged at `warn` and swallowed.

## Budget knobs

| Field                   | Default      | Meaning |
|-------------------------|--------------|---------|
| `context_budget`        | 8,000 tokens | Total window for messages + memories. |
| `memory_budget_fraction`| 0.1 (10%)    | Share of the budget reserved for recalled memories. |
| `recall_k`              | 5            | Top-k memories fetched per request. |

## Startup log line

On boot, Coulisse prints the memory config it resolved:

```text
  memory: sqlite at ./coulisse-memory.db; embedder=openai / text-embedding-3-small
  extractor: anthropic / claude-haiku-4-5-20251001 (dedup_threshold=0.9, max_facts_per_turn=5)
```

Or when the extractor is off:

```text
  extractor: disabled (memory only grows via explicit API calls)
```

## Example configs

### OpenAI end-to-end

```yaml
providers:
  openai:
    api_key: sk-...

memory:
  embedder:
    provider: openai
    model: text-embedding-3-small
  extractor:
    provider: openai
    model: gpt-4o-mini
```

### Anthropic completions + Voyage embeddings

```yaml
providers:
  anthropic:
    api_key: sk-ant-...

memory:
  embedder:
    provider: voyage
    model: voyage-3.5
    api_key: pa-...          # Voyage is not under providers: so set the key here
  extractor:
    provider: anthropic
    model: claude-haiku-4-5-20251001
```

### Offline dev — no external calls

```yaml
memory:
  backend:
    kind: in_memory          # ephemeral; evaporates on restart
  embedder:
    provider: hash
  # no extractor, no embeddings API calls, no persistence
```
