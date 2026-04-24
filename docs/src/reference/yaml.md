# YAML schema

A complete reference for every field in `coulisse.yaml`.

## Top-level

```yaml
agents: [ ... ]               # required, non-empty
default_user_id: <string>     # optional, unset by default
mcp: { ... }                  # optional
providers: { ... }            # required
```

## `default_user_id`

- **Type:** string
- **Default:** unset
- **Purpose:** fallback identifier for requests that don't supply `safety_identifier` (or the deprecated `user`).

Leave it unset for multi-tenant deployments — unidentified requests will be rejected. Set it to something like `"main"` for local or single-user setups so memory still works whether or not the client bothers to send an id. See [User identification](../configuration/user-id.md).

## `providers`

- **Type:** map of `provider_kind → provider_config`
- **Required.** At least one provider must be declared.

### Supported keys

`anthropic`, `cohere`, `deepseek`, `gemini`, `groq`, `openai`.

### Per-provider fields

| Field     | Type   | Required | Notes |
|-----------|--------|----------|-------|
| `api_key` | string | yes      | Provider API key. |

```yaml
providers:
  anthropic:
    api_key: sk-ant-...
  openai:
    api_key: sk-...
```

## `mcp`

- **Type:** map of `server_name → server_config`
- **Optional.** Omit if you don't use tools.

Server names are arbitrary — they're what agents refer to under `mcp_tools`.

### Common fields

| Field       | Type   | Required | Notes |
|-------------|--------|----------|-------|
| `transport` | enum   | yes      | `stdio` or `http`. |

### `transport: stdio`

| Field     | Type       | Required | Notes |
|-----------|------------|----------|-------|
| `command` | string     | yes      | Executable to run. |
| `args`    | list<str>  | no       | Command-line arguments. |
| `env`     | map<str,str> | no     | Environment variables for the child. |

### `transport: http`

| Field | Type   | Required | Notes |
|-------|--------|----------|-------|
| `url` | string | yes      | Streamable-HTTP MCP endpoint. |

### Examples

```yaml
mcp:
  hello:
    transport: stdio
    command: uvx
    args: [--from, git+https://..., hello-mcp-server]

  calculator:
    transport: http
    url: http://localhost:8080
```

## `agents`

- **Type:** list of agent configs
- **Required.** At least one agent must be defined.

### Per-agent fields

| Field        | Type                  | Required | Notes |
|--------------|-----------------------|----------|-------|
| `name`       | string                | yes      | Unique agent identifier; clients pass this as `model`. |
| `provider`   | string                | yes      | Key under `providers`. |
| `model`      | string                | yes      | Upstream model identifier. |
| `preamble`   | string                | no       | System prompt. Default: empty. |
| `mcp_tools`  | list<mcp_tool_access> | no       | Tools this agent may use. |

### `mcp_tools` entry

| Field   | Type       | Required | Notes |
|---------|------------|----------|-------|
| `server`| string     | yes      | Key under `mcp`. |
| `only`  | list<str>  | no       | Allowed tool names. Omit for full access. |

### Complete agent example

```yaml
agents:
  - name: code-reviewer
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: |
      You are a thorough code reviewer.
    mcp_tools:
      - server: filesystem
        only:
          - read_file
      - server: hello
```

## Validation

On startup, Coulisse checks:

- There is at least one agent.
- Agent names are unique.
- Every agent's `provider` is configured.
- Every referenced MCP server is configured.

Any violation fails fast with an error message that names the offending agent and field.
