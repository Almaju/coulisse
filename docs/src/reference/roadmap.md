# Roadmap

What's in Coulisse today, and what's coming.

## Working today

- Multi-agent routing via the `model` field.
- Per-user conversation history with isolation.
- Long-term memory with semantic recall.
- Multi-backend support (Anthropic, OpenAI, Gemini, Cohere, Deepseek, Groq).
- OpenAI-compatible HTTP API (`/v1/chat/completions`, `/v1/models`).
- Read-only admin UI at `/admin` for browsing conversations and memories.
- Streaming responses over SSE (`stream: true`, with `stream_options.include_usage`).
- MCP tool integration over stdio and HTTP, with per-agent filtering.
- Per-user token rate limiting (hour / day / month).
- YAML-driven config with startup validation.

## Planned

### Durable rate-limit state

Current rate-limit counters live in memory — they reset on restart and don't span multiple instances. A durable, shared backend is planned so quotas survive reboots and horizontal scaling.

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
