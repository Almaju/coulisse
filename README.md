# Coulisse

**One YAML file. An OpenAI-compatible server with memory, tools, and multi-backend routing.**

Coulisse is a single Rust binary that reads a `coulisse.yaml` file and spins up an OpenAI-compatible HTTP server. Point your existing SDKs, IDEs, and tools at it like any other OpenAI endpoint — and everything configurable lives in that one YAML file.

Every multi-agent project ends up re-implementing the same plumbing: per-user memory, multi-backend routing, rate limiting, tool integration, multiple system prompts. Coulisse collapses that plumbing into one configurable server.

## Quickstart

**Build:**

```bash
git clone https://github.com/almaju/coulisse.git
cd coulisse
cargo build --release
```

**Configure:**

```bash
cp coulisse.example.yaml coulisse.yaml
# edit coulisse.yaml and drop in an API key
```

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
    api_key: sk-ant-...
  openai:
    api_key: sk-...

memory:
  backend:
    kind: sqlite
    path: ./coulisse-memory.db
  embedder:
    provider: openai
    model: text-embedding-3-small

mcp:
  hello:
    transport: stdio
    command: uvx
    args: [--from, git+https://github.com/macsymwang/hello-mcp-server.git, hello-mcp-server]

agents:
  - name: claude-assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: You are a helpful general-purpose assistant.
    mcp_tools:
      - server: hello

  - name: gpt-assistant
    provider: openai
    model: gpt-4o
    preamble: You are a helpful general-purpose assistant.
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
| Rate limiting | Per-user token quotas on hour / day / month windows. |
| YAML-driven | Every setting lives in `coulisse.yaml` with startup validation. |

Status: early, but usable for prototypes and personal projects. See the [roadmap](docs/src/reference/roadmap.md) for what's next.

## Documentation

The user guide lives in an mdbook under [`docs/`](docs/). Preview it locally:

```bash
mdbook serve docs --port 4421
```

Then open <http://localhost:4421>.

## Contributing

The repo ships a pre-commit hook at `.githooks/pre-commit` that runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test`. Enable it once per clone:

```bash
git config core.hooksPath .githooks
```

## License

MIT. See [LICENSE](LICENSE).
