# Roadmap

What's in Coulisse today, and what's coming.

## Working today

- Multi-agent routing via the `model` field.
- Per-user conversation history with isolation.
- Long-term memory with semantic recall.
- Multi-backend support (Anthropic, OpenAI, Gemini, Cohere, Deepseek, Groq).
- OpenAI-compatible HTTP API (`/v1/chat/completions`, `/v1/models`).
- MCP tool integration over stdio and HTTP, with per-agent filtering.
- YAML-driven config with startup validation.

## Planned

### Rate limiting

Enforce per-user and per-agent request and token limits from the YAML. Today there's no limiter — run Coulisse behind a gateway if you need one.

### Streaming responses

The request schema already accepts a `stream` flag, but streaming isn't wired to the transport layer yet. Planned for parity with the OpenAI SDK's streaming UX.

### Workflow orchestration

Chaining agents into declarative pipelines (one agent's output feeds the next, with conditional routing) — all configured in YAML rather than app code.

### Persistent memory

Memory today is in-process and evaporates on restart. A persistent backend (SQLite or a pluggable store) is planned so memory survives restarts.

### Real embedder

The current embedder is a hash-based placeholder for development. Swapping in a real embedding model (provider-hosted or local) is required before production use — and is planned as a built-in option.

### Tunable memory budgets

`context_budget`, `memory_budget_fraction`, and `recall_k` live in code today. They'll move into the YAML config, scoped per-agent.

---

This list reflects what's on deck at the time of writing — check the repository for the current state.
