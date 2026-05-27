# Admin REST API

Coulisse exposes an admin REST API on `/mcp-admin/*`. Once configured, any
HTTP client — a shell script, a Claude Code custom slash-command, a Cursor
plugin, or a plain `curl` — can inspect or mutate a live instance:
list agents, peek at the task queue, cancel a stuck task, reset a rate
limit.

This is distinct from [MCP tool integration](./tools.md), where Coulisse
*consumes* external MCP servers as tools for its agents. This endpoint is
for **operating** a running Coulisse instance.

## Auth

The `/mcp-admin` surface has its own bearer-token scope, separate from
`auth.admin` (the studio UI). Add one block to `coulisse.yaml`:

```yaml
auth:
  mcp_admin:
    token: "your-secret-token"   # keep this out of version control
```

Every request must carry the token:

```
Authorization: Bearer your-secret-token
```

Without this block, the `/mcp-admin` routes are not mounted at all —
there is no endpoint to hit, authenticated or not.

> **Why a separate scope?**
> `auth.admin` guards the studio UI (cookie-based). `auth.mcp_admin` is
> bearer-only, designed for programmatic clients. The two scopes never
> cross-authenticate: an `mcp_admin` token does not open `/admin/*`, and
> an `admin` session does not open `/mcp-admin/*`.

## Endpoints

All endpoints return JSON. On error they return `{"error": "…"}` with a
relevant HTTP status code.

### Agents

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/mcp-admin/agents` | List all agents |
| `GET` | `/mcp-admin/agents/:name` | Get one agent by name |
| `POST` | `/mcp-admin/agents/:name` | Hot-patch an agent config (**volatile**) |

`POST /mcp-admin/agents/:name` accepts a JSON body with the new agent
config and applies it **in memory only**. The response includes a
warning:

```json
{
  "name": "my-agent",
  "persistent": false,
  "warning": "volatile — changes are lost on Coulisse restart; edit coulisse.yaml to make them permanent"
}
```

### Tasks

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/mcp-admin/tasks` | Queue snapshot (default: 50 most recent) |
| `GET` | `/mcp-admin/tasks?limit=N&state=errored` | Filter by state, cap results |
| `GET` | `/mcp-admin/tasks/:id` | Full task detail |
| `POST` | `/mcp-admin/tasks/:id/cancel` | Mark task as `errored` |
| `POST` | `/mcp-admin/tasks/:id/requeue` | Re-enqueue an errored task |

`cancel` writes the state `errored` unconditionally. If a worker claims
the task between your call and the write, it may finish before the cancel
lands. A proper pre-emption mechanism is planned for a later version.

`requeue` only accepts tasks currently in `errored` state. Any other
state returns `422 Unprocessable Entity`.

### Rate limits

| Method | Path | Description |
|--------|------|-------------|
| `DELETE` | `/mcp-admin/rate-limits/:user_id?confirm=true` | Reset all counters for a user |

`?confirm=true` is required — omitting it returns `400 Bad Request`.
Wildcards and empty strings are rejected to prevent accidental bulk resets.

## Example: curl

```bash
TOKEN="your-secret-token"
BASE="https://your-coulisse-host"

# List agents
curl -H "Authorization: Bearer $TOKEN" $BASE/mcp-admin/agents

# Cancel a task
curl -X POST -H "Authorization: Bearer $TOKEN" \
  $BASE/mcp-admin/tasks/550e8400-e29b-41d4-a716-446655440000/cancel

# Requeue a failed task
curl -X POST -H "Authorization: Bearer $TOKEN" \
  $BASE/mcp-admin/tasks/550e8400-e29b-41d4-a716-446655440000/requeue

# Reset rate limits for a specific user
curl -X DELETE -H "Authorization: Bearer $TOKEN" \
  "$BASE/mcp-admin/rate-limits/user-42?confirm=true"
```

## Audit trail

Every write call (`POST`, `DELETE`) emits a structured log event via
`tracing`:

- `tool` — the operation (`update_agent`, `cancel_task`, `requeue_task`,
  `reset_rate_limit`)
- `caller` — SHA-256 hash of the Authorization header (never the token
  itself)
- `outcome` — `ok` or `error`

These events appear in your log stream alongside all other Coulisse traces.

## YAML reference

```yaml
auth:
  # Studio UI — cookie-based, unchanged
  admin:
    basic:
      username: admin
      password: "studio-secret"

  # Admin REST API — bearer-only, independent scope
  mcp_admin:
    token: "mcp-secret"
```

Omit `mcp_admin` entirely to disable the endpoint. The two scopes are
fully independent.
