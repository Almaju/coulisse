# Admin MCP server

Coulisse can expose its own admin surface as an MCP server. Once configured,
a client like Claude Code or Cursor can connect and inspect or mutate a live
instance in plain language — list agents, tail events, cancel a stuck task,
hot-reload config.

This is distinct from [MCP tool integration](./tools.md), where Coulisse
*consumes* external MCP servers as tools for its agents. Here, Coulisse
*is* the MCP server.

## Transport

Two transports are supported:

| Transport | Use case |
|-----------|----------|
| **stdio** | Local dev — Claude Code or Cursor on the same machine |
| **HTTP/SSE** on `/mcp-admin` | Remote instances (staging, a server you SSH into) |

## Auth

The admin MCP surface has its own bearer-token scope, separate from
`auth.admin` (the studio UI). Add one section to `coulisse.yaml`:

```yaml
auth:
  mcp_admin:
    token: "your-secret-token"
```

The token is checked on every MCP request. Without this block, the
`/mcp-admin` route is not mounted at all.

> **Why a separate scope?**
> You may want to give an LLM client access to the admin MCP without
> giving it access to the studio HTTP interface, or vice-versa. The two
> scopes never cross-authenticate.

## Connecting a client

### Claude Code (stdio)

In your Claude Code settings, add:

```json
{
  "mcpServers": {
    "coulisse-admin": {
      "command": "coulisse",
      "args": ["admin-mcp", "stdio"],
      "cwd": "/path/to/your/coulisse"
    }
  }
}
```

### Claude Code (HTTP)

If your Coulisse instance is on a remote server:

```json
{
  "mcpServers": {
    "coulisse-admin": {
      "type": "sse",
      "url": "https://your-coulisse-host/mcp-admin",
      "headers": {
        "Authorization": "Bearer your-secret-token"
      }
    }
  }
}
```

### Cursor

Same as Claude Code — Cursor uses the same MCP config format.

## Available tools

### Read-only

| Tool | Description |
|------|-------------|
| `list_agents` | All agents: name, provider, model, queue depth, last activity |
| `get_agent(name)` | Full config + recent telemetry for one agent |
| `list_conversations(filter?)` | Recent conversations, filterable by agent or user |
| `get_conversation(id)` | Turns, tool calls, and judge scores for one conversation |
| `tail_events(since?, room?, agent?)` | Telemetry pull — equivalent to `/admin/live` in snapshot form |
| `list_tasks(state?)` | Queue snapshot; filter by `queued`, `running`, `done`, `errored` |
| `get_task(id)` | Full task detail + associated logs |

### Write

| Tool | Description |
|------|-------------|
| `reload_config` | Hot-reload `coulisse.yaml` (same as `SIGHUP`) |
| `update_agent(name, patch)` | Patch a field on an agent **in memory only** — volatile, lost on next reload |
| `cancel_task(id)` | Mark a task as errored with reason `"cancelled via mcp_admin"` |
| `requeue_task(id)` | Re-enqueue an errored task |
| `reset_rate_limit(scope)` | Clear the rate-limit window for a specific user_id |

> **`update_agent` is volatile.** The YAML file is not touched. The patch
> survives until the next `reload_config` or process restart. Use it to
> test a prompt change quickly; then commit it to `coulisse.yaml` when
> happy.

> **`cancel_task` note (v1).** The cancellation writes `errored` state
> unconditionally. If a worker picks up the task between your call and
> the write, the task may finish successfully before the cancel lands.
> A proper pre-emption mechanism is planned for a later release.

## Audit trail

Every write tool call emits a telemetry event with:
- `tool` — the tool name called
- `caller` — a hash of the bearer token (never the token itself)
- `outcome` — `ok` or `err`

These events are stored in the same SQLite telemetry store as all other
events, visible in the studio UI under `/admin/live`.

## YAML reference

```yaml
auth:
  # Studio UI — unchanged
  admin:
    token: "studio-secret"

  # Admin MCP — independent scope
  mcp_admin:
    token: "mcp-secret"       # bearer token clients must send
```

Omit `mcp_admin` entirely if you don't want the MCP admin surface.
The two scopes are independent — having one does not imply the other.
