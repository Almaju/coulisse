# Knowledge & RAG

The `knowledge:` block lets agents search a corpus of documents without any external service. Coulisse indexes your sources at startup and exposes a `search_<name>` tool to every agent that needs it.

**Nothing to install.** The default backend uses the same SQLite file as the rest of memory, and the default embedder runs fully offline.

## Quick start

```yaml
knowledge:
  - source: ./docs
```

With just this, Coulisse:

- Chunks every `.md`, `.txt`, and `.pdf` file under `./docs`.
- Embeds the chunks locally (BGE-small-EN-v1.5, ~130 MB, downloaded once on first run).
- Stores the index alongside your memory database.
- Exposes a `search_docs` tool to any agent that lists it under `tools:`.

## Naming and the generated tool

Each knowledge source gets a `name`. If you omit it, Coulisse derives one from the `source` path.

```yaml
knowledge:
  - name: rust_book
    source: ./rust-book
```

The name becomes the suffix of the generated tool: `name: rust_book` → tool `search_rust_book`.

### Naming rules

Names follow the `[a-z0-9_]` slug format (slug itself max 57 chars, so the generated tool name `search_<slug>` stays within 64 chars):

- Lowercased automatically.
- Hyphens and slashes replaced by underscores.
- Any other non-alphanumeric character is also replaced by an underscore.
- Startup fails with a clear error if the result is empty or longer than 57 characters (which would make the generated tool name exceed 64 characters).

This keeps tool names compatible with every client that calls your agents, including strict MCP clients that only accept `[a-z0-9_]`.

**Examples:**

| `name` in YAML  | Normalized slug | Generated tool        |
|-----------------|------------------|-----------------------|
| `rust-book`     | `rust_book`      | `search_rust_book`    |
| `docs/v2/api`   | `docs_v2_api`    | `search_docs_v2_api`  |
| *(omitted)*     | derived from `source` path | e.g. `search_docs` |

### Tool signature

```
search_<name>(query: string, limit?: int) → [{ text, source, score }]
```

`limit` defaults to 5. `score` is a cosine similarity in `[0.0, 1.0]`.

## Full example

```yaml
knowledge:
  - name: internal_docs
    source: ./docs
    strategy: chunk       # chunk | page | line

embeddings:
  provider: local         # local | openai | ollama
  model: bge-small-en-v1.5
```

### `knowledge` fields

| Field      | Type   | Required | Default   | Notes |
|------------|--------|----------|-----------|-------|
| `name`     | string | no       | derived from `source` | Slug `[a-z0-9_]`, max 57 chars (tool name = `search_<slug>`, must fit in 64). |
| `source`   | string | yes      | —         | Local directory or file path. |
| `strategy` | enum   | no       | `chunk`   | How source files are split: `chunk`, `page`, or `line`. |

### `embeddings` fields

| Field      | Type   | Required | Default              | Notes |
|------------|--------|----------|----------------------|-------|
| `provider` | enum   | no       | `local`              | `local`, `openai`, or `ollama`. |
| `model`    | string | no       | `bge-small-en-v1.5`  | Ignored when `provider: local`. |

The two blocks are independent: `knowledge:` says *what to index*, `embeddings:` says *how to encode it*.

## Indexing and model changes

The index is persisted next to your memory database. Model metadata is stored with the index, so Coulisse detects mismatches.

- **First run:** the local model is downloaded (~130 MB) with a log message. Subsequent starts use the cached file.
- **Model mismatch:** if you change `embeddings.model` after indexing, Coulisse refuses to start and tells you:

  ```
  error: index was built with 'bge-small-en-v1.5', config requests 'text-embedding-3-small'
  → re-run with --reindex to rebuild
  ```

  Pass `--reindex` to drop and rebuild the index. No silent corruption.

## Exposing the tool to an agent

Add the generated tool name under the agent's `tools:` (coming field — see issue #65 for tracking). Until then, the tool is auto-exposed to all agents that have `knowledge:` configured.

## `embeddings` providers

### `local` (default)

- Model: BGE-small-EN-v1.5 via `fastembed`.
- Runs fully offline. ~130 MB ONNX model downloaded on first use.
- ~10 ms per chunk on CPU — fast enough for typical doc sets at startup.
- No API key needed.

### `openai`

Uses the key from your top-level `providers.openai` entry.

```yaml
embeddings:
  provider: openai
  model: text-embedding-3-small
```

### `ollama`

Uses the Ollama instance configured under `providers.ollama`.

```yaml
embeddings:
  provider: ollama
  model: nomic-embed-text
```

## What's out of scope (v1)

- Re-indexation without `--reindex` (no filesystem watch).
- Remote sources (`s3://`, `https://`) — waiting on `storage:` support (#56).
- Incremental indexing.
- Re-ranking (cross-encoder).
- Semantic memory over conversation history — separate ticket, v2.
