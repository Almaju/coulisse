# MCP tools

Coulisse can borrow tools from [Model Context Protocol](https://modelcontextprotocol.io) servers and hand them to your agents. Two transports are supported:

- **stdio** — Coulisse spawns a local command and talks to it over stdin/stdout.
- **http** — Coulisse connects to a running Streamable-HTTP MCP endpoint.

## Declaring MCP servers

Add an `mcp` section with a named entry per server:

```yaml
mcp:
  hello:
    transport: stdio
    command: uvx
    args:
      - --from
      - git+https://github.com/macsymwang/hello-mcp-server.git
      - hello-mcp-server

  calculator:
    transport: http
    url: http://localhost:8080
```

### stdio fields

- `transport: stdio`
- `command` (required) — the executable to spawn (`uvx`, `python`, `node`, …)
- `args` (optional) — arguments to pass
- `env` (optional) — environment variables for the child process

```yaml
mcp:
  my-tool:
    transport: stdio
    command: python
    args: [-m, my_mcp_server]
    env:
      DEBUG: "1"
      API_KEY: abc123
```

### http fields

- `transport: http`
- `url` (required) — the endpoint URL

## Granting tool access to agents

An agent only sees tools you explicitly give it. Reference the server name under `mcp_tools`:

```yaml
agents:
  - name: helper
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    mcp_tools:
      - server: hello           # all tools from "hello"
```

Restrict to a subset with `only`:

```yaml
    mcp_tools:
      - server: hello
        only:
          - say_hello           # only this tool, nothing else
```

## Discovering tool names

On startup Coulisse connects to each MCP server and logs the tools it discovered. Tool names in your `only` list must match what the server advertises — check the startup output or the server's own docs.

## How tool calls work

When a request arrives for an agent with tools:

1. Coulisse collects the agent's allowed tools from the MCP servers.
2. It forwards them to the model as tool definitions.
3. If the model calls a tool, Coulisse dispatches to the MCP server and feeds the result back.
4. This loops until the model produces a final answer (up to 8 turns).

Your client doesn't see any of this — the tool loop is invisible, and only the final assistant message is returned.

See [MCP tool integration](../features/tools.md) for a full walkthrough.
