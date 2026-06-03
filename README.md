# Coulisse

**One YAML file. An OpenAI-compatible server with memory, tools, and multi-backend routing.**

Coulisse is a single Rust binary that reads a `coulisse.yaml` file and spins up an OpenAI-compatible HTTP server. Point your existing SDKs, IDEs, and tools at it like any other OpenAI endpoint — and everything configurable lives in that one YAML file.

Every multi-agent project ends up re-implementing the same plumbing: per-user memory, multi-backend routing, rate limiting, tool integration, multiple system prompts. Coulisse collapses that plumbing into one configurable server.

## Quickstart

**Install** — grab the latest release for your platform:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Almaju/coulisse/releases/latest/download/coulisse-installer.sh | sh
```

Or build from source:

```bash
git clone https://github.com/Almaju/coulisse.git
cd coulisse
cargo build --release
```

**Configure:**

```bash
coulisse init            # writes a minimal coulisse.yaml (one agent, one provider)
coulisse init --from-example  # full annotated example with every section
# edit coulisse.yaml and drop in an API key
```

See [`examples/`](examples/) for realistic multi-agent configs with MCP tools, judges, and triggers.

**Run:**

```bash
./target/release/coulisse
# coulisse listening on http://0.0.0.0:8421
```

**Call it like OpenAI:**

```bash
curl http://localhost:8421/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-assistant",
    "safety_identifier": "user-123",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

Any OpenAI SDK works — just set `base_url` to `http://localhost:8421/v1`.

## What a config looks like

```yaml
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
  openai:
    api_key: ${OPENAI_API_KEY}

vars:
  footer: "Team wiki: https://wiki.example.com"

memory:
  storage: ./coulisse-memory.db
  user_state: true

mcp:
  hello:
    transport: stdio
    command: uvx
    args: [--from, git+https://github.com/macsymwang/hello-mcp-server.git, hello-mcp-server]

agents:
  - name: claude-assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: |
      You are a helpful general-purpose assistant.
      ${vars.footer}
    mcp_tools:
      - server: hello

  - name: gpt-assistant
    provider: openai
    model: gpt-4o
    preamble: You are a helpful general-purpose assistant.

triggers:
  - name: daily-check
    type: cron
    schedule: "0 9 * * *"
    agent: claude-assistant
    prompt: "Good morning. Summarise yesterday in five bullets."
```

Request a specific agent by setting `model` to its name. Conversation history is kept per `safety_identifier`.

## Features

| | |
|---|---|
| Multi-agent routing | Each agent has its own provider, model, preamble, and tools — pick one via the `model` field. |
| Per-user memory | Isolated conversation history scoped by `safety_identifier`, with semantic recall. |
| Multi-backend | Anthropic, OpenAI, Gemini, Cohere, Deepseek, Groq. |
| OpenAI-compatible | `/v1/chat/completions` and `/v1/models` — drop-in for any OpenAI SDK. |
| MCP tools | Attach Model Context Protocol servers over stdio or HTTP. Per-agent filtering. |
| Async tasks | `dispatch_task` / `tasks_status` built-in tools; background worker pool with a studio queue view. |
| Triggers | Cron, webhook (`POST /hooks/*`), and boot triggers fire agents without an HTTP client. |
| Sidecars | Declare long-lived helper processes (bridges, listeners) under `sidecars:` with restart policy. |
| Config variables | Top-level `vars:` block — declare a snippet once, splice it anywhere with `${vars.name}`. |
| Rate limiting | Per-user token quotas on hour / day / month windows. |
| Studio UI | `/admin/` — browse history, edit agents live, watch the real-time task board. |
| YAML-driven | Every setting lives in `coulisse.yaml`. `coulisse schema` emits a JSON Schema for IDE validation. |

Status: pre-1.0 — usable for prototypes and personal projects. Minor version
bumps may include breaking changes until 1.0. See the
[changelog](CHANGELOG.md) for what's shipped and the
[roadmap](docs/src/reference/roadmap.md) for what's next.

## Upgrading from v0.1.0

The `memory:` block was reshaped. The old fields `backend`, `embedder`, `extractor`,
`context_budget`, `memory_budget_fraction`, and `recall_k` are gone. Replace the whole
block with:

```yaml
memory:
  storage: ./coulisse-memory.db
  user_state: true   # add this only if you had long-term memory enabled before
```

Run `coulisse migrate` (or just start — the migrator runs at boot). See the
[memory configuration guide](docs/src/configuration/memory.md) for the full schema.

## Documentation

The user guide lives in an mdbook under [`docs/`](docs/). Preview it locally:

```bash
mdbook serve docs --port 4421
```

Then open <http://localhost:4421>.

## Contributing

The repo ships a pre-commit hook at `.githooks/pre-commit` that runs `cargo fmt --check`, `cargo clippy`, `cargo sort-derives --check`, `cargo machete`, and `cargo test`. Enable it once per clone:

```bash
git config core.hooksPath .githooks
```

Install the dev tools (cargo-watch, mdbook) with `just install`. The lint stack uses `cargo fmt`, `cargo clippy`, plus `cargo-sort-derives` and `cargo-machete` — install the latter two via `cargo install cargo-sort-derives cargo-machete --locked` if not already on your system.

## License

MIT. See [LICENSE](LICENSE).
