# MCP tools

Coulisse can borrow tools from [Model Context Protocol](https://modelcontextprotocol.io) servers and hand them to your agents. The config has one rule: **declare what the server is, not what protocol it speaks**. Coulisse infers the transport from the shape of the entry.

## Declaring MCP servers

```yaml
mcp:
  # Remote MCP — just paste the URL. OAuth is auto-enabled.
  todoist:
    url: https://ai.todoist.net/mcp

  # Local stdio MCP — give it a command.
  hello:
    command: uvx
    args:
      - --from
      - git+https://github.com/macsymwang/hello-mcp-server.git
      - hello-mcp-server

  # Plain HTTP MCP without auth — explicit opt-out.
  calculator:
    url: http://localhost:8080
    oauth: false
```

The Todoist entry above is **zero config**: the same UX as ChatGPT. Paste the URL, and Coulisse runs RFC 8414 discovery + RFC 7591 Dynamic Client Registration on first use, mints a per-user connect link, stores the token in the vault.

`oauth: discover` is the string shorthand for `{ mode: discover }`. You only switch to the map form when you need to override scopes or use static credentials:

```yaml
mcp:
  # Override discovered scopes
  custom:
    url: https://example.com/mcp
    oauth:
      scopes: [read:items, write:items]   # mode: discover is implied

  # Pre-registered (static) OAuth credentials — for providers that
  # don't support Dynamic Client Registration
  legacy:
    url: https://internal.example.com/mcp
    oauth:
      mode: static
      authorization_url: https://auth.example.com/authorize
      token_url: https://auth.example.com/token
      client_id: my-client
      client_secret: my-secret
      redirect_uri: http://localhost:8423/mcp/legacy/oauth/callback
```

That's it. No `transport:` field, no shim wrappers, no `npx mcp-remote ...` boilerplate. Coulisse figures out:

- `url:` present → HTTP/SSE transport (SSE if the URL path contains `/sse`, otherwise streamable HTTP).
- `command:` present → stdio transport, with optional `args:` / `env:` for the child process.
- `oauth:` is the only thing you opt into yourself, and only when the server actually needs it.

### Auto-detected transport

The path heuristic: if the URL has an `/sse` path segment (`https://mcp.atlassian.com/v1/sse`), Coulisse uses the older MCP-over-SSE protocol. Everything else uses streamable HTTP. URLs without `/sse` that turn out to be SSE-only will fail with a `Missing sessionId parameter` 404 on first call; switch to the explicit form below.

### stdio config fields

- `command` (required) — the executable to spawn (`uvx`, `python`, `node`, …)
- `args` (optional) — arguments
- `env` (optional) — environment variables

```yaml
mcp:
  my-tool:
    command: python
    args: [-m, my_mcp_server]
    env:
      DEBUG: "1"
      API_KEY: abc123
```

### Explicit `transport:` (legacy / override)

The verbose form still works if you need to override the auto-detection:

```yaml
mcp:
  legacy:
    transport: sse           # one of: http, sse, stdio
    url: https://example.com/v2/endpoint    # despite no /sse segment
```

Existing YAMLs that use `transport:` continue to parse unchanged. New code should prefer the URL-only / command-only form above.

## Per-user OAuth (optional)

MCP servers that require user-delegated credentials (Todoist, Atlassian, GitHub,
Google Drive, etc.) can be configured with an `oauth:` block. Coulisse handles
the authorization flow per-user and injects each user's token automatically at
call time — Alice's token is never reachable by Bob.

Two modes:

### `mode: discover` (recommended for modern MCP servers)

Spec-compliant MCP servers (Todoist, Atlassian, Linear, …) advertise their OAuth
endpoints via `/.well-known/oauth-authorization-server` and accept Dynamic Client
Registration. Coulisse discovers + registers itself lazily, on the first user to
authorise. **No credentials in YAML.**

```yaml
mcp:
  todoist:
    transport: http
    url: https://ai.todoist.net/mcp
    oauth:
      mode: discover
      # scopes: [data:read_write]   # optional override
```

### `mode: static` (for non-DCR providers)

For OAuth providers that require a pre-registered app (GitHub OAuth apps,
classic Atlassian Connect, etc.):

```yaml
mcp:
  github:
    transport: http
    url: https://api.githubcopilot.com/mcp
    oauth:
      mode: static
      authorization_url: https://github.com/login/oauth/authorize
      client_id: "${GH_CLIENT_ID}"
      client_secret: "${GH_CLIENT_SECRET}"
      redirect_uri: https://coulisse.example.com/mcp/github/oauth/callback
      scopes: [repo, read:user]
      token_url: https://github.com/login/oauth/access_token
```

`static` requires: `authorization_url`, `client_id`, `client_secret`,
`redirect_uri`, `token_url`. Missing any of these at startup is a fatal error.

Both modes share the same infrastructure secrets (vault encryption + HMAC).
Coulisse auto-generates them on first boot and persists them in
`.coulisse/secrets.env` — no manual setup needed for local use. Override
with `COULISSE_VAULT_KEY` / `COULISSE_HMAC_KEY` env vars for hosted
deployments. `auth.mcp_consumer_secret` is optional (only gates the admin
`POST /connect-link` endpoint). Set `public_base_url:` at the top level
when Coulisse runs on a public hostname; defaults to
`http://localhost:{port}` for local use.

See [Per-user OAuth for MCP](../features/mcp-oauth.md) for the full flow,
endpoints, secrets resolution, and the security trust-model warning.

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

See [MCP tool integration](../features/tools.md) for a full walkthrough.
