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
- Studio UI at `/admin/` — browse conversations, memories, and judge scores; edit agents, judges, experiments, and smoke tests live; watch the real-time task board at `/admin/live`.
- LLM-as-judge evaluation — background scoring of agent replies against YAML-defined rubrics, with per-judge sampling and per-user persistence.
- Experiments (A/B testing) — wrap multiple agents under one addressable name and route traffic between them with sticky-by-user defaults. Three strategies: `split` (weighted random), `shadow` (primary serves the user, others run in the background and are scored), and `bandit` (epsilon-greedy on a single judge criterion).
- Streaming responses over SSE (`stream: true`, with `stream_options.include_usage`).
- MCP tool integration over stdio and HTTP, with per-agent filtering.
- Per-user OAuth 2.0 for MCP servers (token vault, connect-link flow, per-user session pool).
- Per-user token rate limiting (hour / day / month).
- Triggers — start agents on a schedule (`cron`), via HTTP POST (`webhook`), or on server boot (`boot`).
- Async task queue — `dispatch_task` enqueues background work; `tasks_status` inspects the queue from chat; `/admin/live` shows it in real time.
- Sidecars — long-lived helper processes (bridges, exporters) spawned and supervised by Coulisse.
- Config variables (`vars:`) — named string snippets shared across agent preambles.
- JSON Schema generation (`coulisse schema`) for IDE autocompletion and live validation.
- YAML-driven config with startup validation.
- Docker image with a volume-mounted SQLite store.

- Credential-bound identity — `auth.proxy.identity: from_credential` derives the per-user identity from the authenticated principal (Basic username or OIDC `sub`) instead of trusting the request body, and rejects a mismatched `safety_identifier`. Makes adversarial multi-tenant serving safe; mutually exclusive with `default_user_id`. See [User identification](../configuration/user-id.md#binding-identity-to-the-credential).

## Planned

### Durable rate-limit state

Current rate-limit counters live in memory — they reset on restart and don't span multiple instances. A durable, shared backend is planned so quotas survive reboots and horizontal scaling.

### Vector index for large memory stores

Recall currently does a linear cosine scan over all memories for the user. Fine at hundreds-to-low-thousands of memories per user, but a vector index will be needed if per-user memory counts grow into the tens of thousands.

### Per-agent memory overrides

Today the `memory:` block is global. A future revision will allow per-agent scoping (different embedders or budgets per agent) for cases where one agent handles long-form research and another handles short user chat.

---

This list reflects what's on deck at the time of writing — check the repository for the current state.
