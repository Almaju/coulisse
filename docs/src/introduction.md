# Coulisse

**One YAML file. An OpenAI-compatible server with memory, tools, and multi-backend routing.**

Coulisse is a single Rust binary that reads a `coulisse.yaml` file and spins up an OpenAI-compatible HTTP server. You point your existing tools, SDKs, and projects at it like any other OpenAI endpoint — and everything configurable lives in that one YAML file.

## Why Coulisse?

Every multi-agent project ends up re-implementing the same plumbing:

- Per-user conversation memory
- Routing between model providers
- Rate limits and retries
- Tool integration
- Multiple agents with different system prompts

Coulisse collapses this plumbing into one configurable server. You describe the setup in YAML and pilot the whole thing from there, instead of writing glue code for each prototype.

## How it works

```text
┌──────────────────┐        ┌──────────────────┐        ┌──────────────────┐
│  Your SDK / app  │───────▶│     Coulisse     │───────▶│   Anthropic      │
│  (OpenAI client) │        │                  │        │   OpenAI         │
└──────────────────┘        │   coulisse.yaml  │        │   Gemini …       │
                            │                  │        └──────────────────┘
                            │   + memory       │
                            │   + MCP tools    │        ┌──────────────────┐
                            │   + per-user     │───────▶│   MCP servers    │
                            └──────────────────┘        └──────────────────┘
```

1. Your application talks to Coulisse using any OpenAI-compatible SDK.
2. Coulisse picks the agent you asked for (by model name), assembles the user's memory, and calls the right backend.
3. The response flows back — and the exchange is saved to that user's memory for next time.

## What's in the box

| Feature                 | Status |
|-------------------------|--------|
| Multi-agent routing     | ✅ Working |
| Per-user memory         | ✅ Persistent (SQLite) with semantic recall |
| Real embedders          | ✅ OpenAI + Voyage (hash fallback for offline dev) |
| Auto-extraction         | ✅ Optional — pulls durable facts from each exchange |
| MCP tool integration    | ✅ Working (stdio + HTTP) |
| Multi-backend support   | ✅ Anthropic, OpenAI, Gemini, Cohere, Deepseek, Groq |
| OpenAI-compatible API   | ✅ `/v1/chat/completions`, `/v1/models` |
| Streaming responses     | ✅ Server-Sent Events |
| Rate limiting           | ✅ Per-user token quotas (hour / day / month, in-memory) |
| Studio UI               | ✅ Read-only at `/studio` |
| Workflow orchestration  | ⏳ Planned |
| Durable rate-limit state| ⏳ Planned |

Continue to [Installation](./getting-started/installation.md) to get started.
