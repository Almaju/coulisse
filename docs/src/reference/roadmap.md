# Roadmap

What's in Coulisse today, and what's coming.

## Working today

- Multi-agent routing via the `model` field.
- Agents as tools — expose one agent to another under `subagents:` with a `purpose:` description. Nested invocations are bounded by a depth cap.
- Per-user conversation history with isolation.
- Long-term memory with semantic recall — **persistent via SQLite** and backed by a real embedder (OpenAI or Voyage AI; `hash` fallback for offline dev).
- Auto-extraction — an optional background task pulls durable facts from each exchange and deduplicates them before storing.
- Tunable memory budgets (`context_budget`, `memory_budget_fraction`, `recall_k`) in YAML.
- Multi-backend support (Anthropic, OpenAI, Gemini, Cohere, Deepseek, Groq).
- OpenAI-compatible HTTP API (`/v1/chat/completions`, `/v1/models`).
- Read-only studio UI at `/studio` for browsing conversations, memories, and judge scores.
- LLM-as-judge evaluation — background scoring of agent replies against YAML-defined rubrics, with per-judge sampling and per-user persistence.
- Streaming responses over SSE (`stream: true`, with `stream_options.include_usage`).
- MCP tool integration over stdio and HTTP, with per-agent filtering.
- Per-user token rate limiting (hour / day / month).
- YAML-driven config with startup validation.
- Docker image with a volume-mounted SQLite store.

## Planned

### Durable rate-limit state

Current rate-limit counters live in memory — they reset on restart and don't span multiple instances. A durable, shared backend is planned so quotas survive reboots and horizontal scaling.

### Workflow orchestration

Chaining agents into declarative pipelines (one agent's output feeds the next, with conditional routing) — all configured in YAML rather than app code.

### Vector index for large memory stores

Recall currently does a linear cosine scan over all memories for the user. Fine at hundreds-to-low-thousands of memories per user, but a vector index will be needed if per-user memory counts grow into the tens of thousands.

### Per-agent memory overrides

Today the `memory:` block is global. A future revision will allow per-agent scoping (different embedders or budgets per agent) for cases where one agent handles long-form research and another handles short user chat.

---

This list reflects what's on deck at the time of writing — check the repository for the current state.
