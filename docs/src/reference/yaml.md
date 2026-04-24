# YAML schema

A complete reference for every field in `coulisse.yaml`.

## Top-level

```yaml
agents: [ ... ]               # required, non-empty
default_user_id: <string>     # optional, unset by default
judges: [ ... ]               # optional; empty/omitted = no evaluation
mcp: { ... }                  # optional
memory: { ... }               # optional; defaults to sqlite + hash embedder
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
| `args`    | `list<str>` | no      | Command-line arguments. |
| `env`     | `map<str,str>` | no   | Environment variables for the child. |

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

## `memory`

- **Type:** object
- **Optional.** Omit for defaults (sqlite at `./coulisse-memory.db`, offline `hash` embedder, no auto-extraction).

See [Memory configuration](../configuration/memory.md) for the full walkthrough and examples.

### Sub-fields

| Field                          | Type   | Required | Default                                |
|--------------------------------|--------|----------|----------------------------------------|
| `backend.kind`                 | enum   | no       | `sqlite`                               |
| `backend.path`                 | string | no       | `./coulisse-memory.db`                 |
| `embedder.provider`            | enum   | no       | `hash`                                 |
| `embedder.model`               | string | depends  | required for `openai`/`voyage`         |
| `embedder.api_key`             | string | no       | falls back to `providers.<provider>`    |
| `embedder.dims`                | int    | no       | 32 (hash only)                         |
| `extractor.provider`           | string | yes\*    | — (\* required when `extractor` is set) |
| `extractor.model`              | string | yes\*    | —                                      |
| `extractor.dedup_threshold`    | float  | no       | 0.9                                    |
| `extractor.max_facts_per_turn` | int    | no       | 5                                      |
| `context_budget`               | int    | no       | 8000                                   |
| `memory_budget_fraction`       | float  | no       | 0.1                                    |
| `recall_k`                     | int    | no       | 5                                      |

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
| `judges`     | `list<string>`        | no       | Names of judges (from top-level `judges:`) that evaluate this agent's replies. Empty = no evaluation. |
| `mcp_tools`  | `list<mcp_tool_access>` | no     | Tools this agent may use. |
| `purpose`    | string                | no       | Tool description when this agent is exposed via another agent's `subagents`. Omit for standalone agents; add a concrete one-line description when this agent is meant to be called as a specialist. |
| `subagents`  | `list<string>`        | no       | Names of other agents exposed as callable tools. Each entry must refer to another entry under `agents`. Self-reference and duplicates are rejected at startup. |

### `mcp_tools` entry

| Field   | Type       | Required | Notes |
|---------|------------|----------|-------|
| `server`| string     | yes      | Key under `mcp`. |
| `only`  | `list<str>` | no      | Allowed tool names. Omit for full access. |

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

### Subagent example

```yaml
agents:
  - name: resume_critic
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    purpose: Critique and rewrite a resume for a target role.
    preamble: |
      Given a resume and a target role, return a revised resume
      and a bullet list of the biggest gaps.

  - name: coach
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    subagents: [resume_critic]
    preamble: |
      Delegate resume work to `resume_critic` when relevant.
```

See [Multi-agent routing](../features/multi-agent.md) for the full subagent walkthrough.

## `judges`

- **Type:** list of judge configs
- **Optional.** Omit (or leave empty) for no automatic evaluation.

Judges are background LLM-as-judge evaluators. An agent opts in by listing judge names in its own `judges:` field. See [LLM-as-judge evaluation](../features/evaluation.md) for the full walkthrough.

### Per-judge fields

| Field           | Type              | Required | Default | Notes |
|-----------------|-------------------|----------|---------|-------|
| `name`          | string            | yes      | —       | Unique judge identifier; agents refer to it here. |
| `provider`      | string            | yes      | —       | Must match a key under `providers`. |
| `model`         | string            | yes      | —       | Upstream model identifier for the judge call. |
| `rubrics`       | `map<string,string>` | yes   | —       | `criterion: short description of what to assess`. One score row per criterion per scored turn. Must declare at least one entry. |
| `sampling_rate` | float             | no       | `1.0`   | In `[0.0, 1.0]`. `1.0` = every turn, `0.1` ≈ 10%, `0.0` = never. |

Rubric descriptions should say **what** to evaluate — don't include scale, JSON, or format instructions. Coulisse forces the output shape internally (integer 0-10 per criterion with a one-sentence reasoning).

### Example

```yaml
judges:
  - name: quality
    provider: openai
    model: gpt-4o-mini
    sampling_rate: 1.0
    rubrics:
      accuracy:     Factual accuracy. Flag hallucinations.
      helpfulness:  Whether the assistant answered the user's question.
      tone:         Politeness and tone.
```

## Validation

On startup, Coulisse checks:

- There is at least one agent.
- Agent names are unique.
- Every agent's `provider` is configured.
- Every referenced MCP server is configured.
- Every name in `subagents` refers to another defined agent.
- No agent lists itself under `subagents`.
- `subagents` entries are unique within an agent (no duplicates).
- Every referenced judge exists.
- Judge names are unique.
- Every judge's `provider` is configured and supported.
- Every judge has at least one rubric.
- Every judge's `sampling_rate` is in `[0.0, 1.0]`.

Any violation fails fast with an error message that names the offending agent or judge and field.
