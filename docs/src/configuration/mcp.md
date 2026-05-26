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

## Per-user OAuth (optional)

MCP servers that require user-delegated credentials (Jira, GitHub, Google Drive, etc.)
can be configured with an `oauth:` block. Coulisse will handle the authorization flow
and inject each user's token automatically at call time.

```yaml
mcp:
  github:
    transport: stdio
    command: uvx
    args: [github-mcp-server]
    oauth:
      authorization_url: https://github.com/login/oauth/authorize
      client_id: "${GH_CLIENT_ID}"
      client_secret: "${GH_CLIENT_SECRET}"
      redirect_uri: https://coulisse.example.com/mcp/github/oauth/callback
      scopes: [repo, read:user]
      token_url: https://github.com/login/oauth/access_token
```

Required `oauth:` fields: `authorization_url`, `client_id`, `client_secret`, `redirect_uri`,
`token_url`. Missing any of these at startup is a fatal error. Also requires
`auth.mcp_consumer_secret` in the `auth:` block, plus the `COULISSE_VAULT_KEY` and
`COULISSE_HMAC_KEY` environment variables.

See [Per-user OAuth for MCP](../features/mcp-oauth.md) for the full flow, required
environment variables, and the security trust-model warning.

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

On startup Coulisse connects to each non-OAuth MCP server and logs the tools it discovered. OAuth-enabled servers connect per-user on first use. Tool names in your `only` list must match what the server advertises — check the startup output or the server's own docs.

## How tool calls work

When a request arrives for an agent with tools:

1. Coulisse collects the agent's allowed tools from the MCP servers.
2. It forwards them to the model as tool definitions.
3. If the model calls a tool, Coulisse dispatches to the MCP server and feeds the result back.
4. This loops until the model produces a final answer (up to 8 turns by default, configurable via the agent's `max_turns` field).

Your client doesn't see any of this — the tool loop is invisible, and only the final assistant message is returned.

See [MCP tool integration](../features/tools.md) for a full walkthrough. For the Matrix integration story (which is *not* MCP-based — it's an external bridge), see [Matrix as the chat UI](../features/matrix-chat.md).
