# Roadmap

What's in Coulisse today, and what's coming.

## Working today

- Multi-agent routing via the `model` field.
- Agents as tools — expose one agent to another under `subagents:` with a `purpose:` description. Nested invocations are bounded by a depth cap.
- Per-user conversation history with isolation.
- Long-term memory with semantic recall — **persistent via SQLite** and backed by a real embedder (OpenAI or Voyage AI; `hash` fallback for offline dev).
- Long-term user state — opt-in `user_state: true` enables a background extractor that pulls durable facts from each exchange and deduplicates them before storing. Embedder and extraction model are auto-derived from your configured providers.
- Multi-backend support (Anthropic, OpenAI, Gemini, Cohere, Deepseek, Groq).
- OpenAI-compatible HTTP API (`/v1/chat/completions`, `/v1/models`).
- Streaming responses over SSE (`stream: true`, with `stream_options.include_usage`), including subagent `handoff_started` events and 20-second heartbeats.
- Async tasks — `dispatch_task` tool enqueues fire-and-forget agent runs; a worker pool drains the queue; `tasks_status` tool lets agents inspect the queue from chat.
- Triggers — agents that wake up without a waiting user: `cron` (POSIX schedule), `webhook` (any HTTP POST to `/hooks/<path>`), and `boot` (once at startup).
- Sidecars — long-lived helper processes declared under `sidecars:` in YAML, spawned and supervised alongside Coulisse (crash restart, log capture, env injection).
- Per-user OAuth 2.0 for MCP servers — token vault (AES-256-GCM), connect-link flow, per-user session pool (LRU, 30-min idle timeout).
- Read-only studio UI at `/admin/` for browsing conversations, memories, and judge scores, plus a live activity board at `/admin/live` (tasks + recent tool calls, auto-refreshing).
- LLM-as-judge evaluation — background scoring of agent replies against YAML-defined rubrics, with per-judge sampling and per-user persistence.
- Experiments (A/B testing) — wrap multiple agents under one addressable name and route traffic between them with sticky-by-user defaults. Three strategies: `split` (weighted random), `shadow` (primary serves the user, others run in the background and are scored), and `bandit` (epsilon-greedy on a single judge criterion).
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
