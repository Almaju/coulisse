# YAML schema

A complete reference for every field in `coulisse.yaml`.

## Top-level

```yaml
agents: [ ... ]               # required, non-empty
auth: { ... }                 # optional; per-scope auth for /v1/* and /admin/*
default_user_id: <string>     # optional, unset by default
experiments: [ ... ]          # optional; A/B test groups over agents
judges: [ ... ]               # optional; empty/omitted = no evaluation
mcp: { ... }                  # optional
memory: { ... }               # optional; defaults to sqlite + hash embedder
providers: { ... }            # required
telemetry: { ... }            # optional; fmt + sqlite on by default, OTLP opt-in
```

## `auth`

- **Type:** object
- **Optional.** Omit to leave both surfaces unauthenticated (fine for local dev, never for anything exposed beyond loopback).

Two independent scopes:

- `auth.proxy` guards the OpenAI-compatible `/v1/*` surface that SDK clients call.
- `auth.admin` guards the `/admin/*` surface (the studio UI).

Each scope is itself optional and accepts the same shape: exactly one of `basic` or `oidc` when present. They are mutually exclusive within a scope — the server rejects a scope block that has both or neither. The two scopes are independent, so you can enable Basic on one and OIDC on the other.

### `auth.<scope>.basic`

Static HTTP Basic credentials. Best for local dev or a single-operator deployment.

| Field      | Type   | Required | Default  | Notes |
|------------|--------|----------|----------|-------|
| `password` | string | yes      | —        | Non-empty. Rotate if suspected leaked — there's no token revocation. |
| `username` | string | no       | `admin`  | Non-empty when set. |

```yaml
auth:
  admin:
    basic:
      password: choose-something-strong
      username: admin
```

### `auth.<scope>.oidc`

Authorization-code-with-PKCE login against an OIDC-compliant IdP (Authentik, Keycloak, Auth0, Google, etc.). Access control is delegated to the IdP's application policy — Coulisse accepts any successfully authenticated user.

| Field           | Type            | Required | Default           | Notes |
|-----------------|-----------------|----------|-------------------|-------|
| `client_id`     | string          | yes      | —                 | Must match the client registered at the IdP. |
| `client_secret` | string          | no       | —                 | Required for confidential clients (Authentik's default); omit for public clients using PKCE only. |
| `issuer_url`    | string          | yes      | —                 | IdP issuer. For Authentik: `https://<host>/application/o/<app-slug>/`. |
| `redirect_url`  | string          | yes      | —                 | Public base URL inside the protected scope. Must be registered as the redirect URI at the IdP. axum-oidc allows every subpath of this URL as a valid redirect. |
| `scopes`        | `list<string>`  | no       | `[email, profile]`| Extra OAuth2 scopes. `openid` is added automatically. |

```yaml
auth:
  admin:
    oidc:
      issuer_url:    https://authentik.example.com/application/o/coulisse/
      client_id:     coulisse-admin
      client_secret: <secret>
      redirect_url:  http://localhost:8421/admin/
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

## `experiments`

- **Type:** list of experiment configs
- **Optional.** Omit (or leave empty) to skip A/B testing.

An experiment wraps two or more agents under one addressable name. Clients send the experiment's `name` in the `model` field and the router picks a variant per request. Experiment names share the agent namespace — collisions are rejected at startup.

See [Experiments](../features/experiments.md) for the end-to-end walkthrough.

### Per-experiment fields

| Field                    | Type             | Required          | Default          | Notes |
|--------------------------|------------------|-------------------|------------------|-------|
| `bandit_window_seconds`  | int              | no (`bandit`)     | `604800` (7 d)   | Bandit-only. Maximum age of scores included in mean-arm computations. |
| `epsilon`                | float            | no (`bandit`)     | `0.1`            | Bandit-only. Probability in `[0.0, 1.0]` of routing to a random arm instead of the leader. |
| `metric`                 | string           | yes (`bandit`)    | —                | Bandit-only. `judge.criterion` to optimise. The judge must declare the criterion in its rubrics, and every variant must opt into the judge. |
| `min_samples`            | int              | no (`bandit`)     | `30`             | Bandit-only. Each arm must accumulate this many scores before exploitation is allowed. |
| `name`                   | string           | yes               | —                | Addressable name; must not collide with any agent name. |
| `primary`                | string           | yes (`shadow`)    | —                | Shadow-only. Variant agent that serves the user. Must be one of `variants`. |
| `purpose`                | string           | no                | —                | Tool description when the experiment is exposed via another agent's `subagents:`. |
| `sampling_rate`          | float            | no (`shadow`)     | `1.0`            | Shadow-only. Probability in `[0.0, 1.0]` that a turn also runs the non-primary variants in the background. |
| `sticky_by_user`         | bool             | no                | `true`           | When `true`, the same user always lands on the same variant (deterministic hash, no DB writes). |
| `strategy`               | enum             | yes               | —                | `split`, `shadow`, or `bandit`. |
| `variants`               | `list<variant>`  | yes               | —                | Non-empty. Each entry references an agent. |

### `variants` entry

| Field    | Type   | Required | Default | Notes |
|----------|--------|----------|---------|-------|
| `agent`  | string | yes      | —       | Name of an agent declared under top-level `agents:`. Variants must reference concrete agents — nesting an experiment is rejected. |
| `weight` | float  | no       | `1.0`   | Strictly positive. Normalised against the sum of all variant weights. |

### Example

```yaml
agents:
  - name: assistant-sonnet
    provider: anthropic
    model: claude-sonnet-4-5-20250929
  - name: assistant-gpt
    provider: openai
    model: gpt-4o

experiments:
  - name: assistant
    strategy: split
    variants:
      - agent: assistant-sonnet
        weight: 0.5
      - agent: assistant-gpt
        weight: 0.5
```

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

## `telemetry`

- **Type:** object
- **Optional.** Omit and Coulisse runs with stderr fmt logs at `info` plus the SQLite mirror that drives the studio UI; no external traces.

The block has three sub-sections — `fmt`, `sqlite`, and `otlp` — each independently toggleable. See [Telemetry configuration](../configuration/telemetry.md) for the full schema and [Telemetry & OpenTelemetry](../features/telemetry.md) for span semantics and OTLP backend integration.

```yaml
telemetry:
  fmt:
    enabled: true        # default
  sqlite:
    enabled: true        # default; powers the studio UI
  otlp:                  # absent = no external traces
    endpoint: "http://localhost:4317"
    protocol: grpc       # or http_binary
    service_name: coulisse
    headers:
      authorization: "Bearer ${OTEL_API_KEY}"
```

## Validation

On startup, Coulisse checks:

- Each present `auth` scope (`proxy`, `admin`) declares exactly one of `basic` or `oidc`.
- `auth.<scope>.basic.password` and `auth.<scope>.basic.username` are non-empty.
- `auth.<scope>.oidc.client_id`, `issuer_url`, and `redirect_url` are non-empty.
- There is at least one agent.
- Agent names are unique.
- Every agent's `provider` is configured.
- Every referenced MCP server is configured.
- Every name in `subagents` refers to a defined agent or experiment.
- No agent lists itself under `subagents`.
- `subagents` entries are unique within an agent (no duplicates).
- Experiment names are unique and do not collide with any agent name.
- Each experiment declares at least one variant.
- Each variant references a defined agent and has a strictly positive `weight`.
- Variant agents within an experiment are unique.
- Strategy-specific fields are only set on the matching strategy (e.g. `primary` only on `shadow`, `metric` only on `bandit`).
- For `shadow`: `primary` is set and matches one of the variants; `sampling_rate` is in `[0.0, 1.0]`.
- For `bandit`: `metric` is `judge.criterion`; the judge exists, declares the criterion in its rubrics, and every variant opts into the judge; `epsilon` is in `[0.0, 1.0]`.
- Every referenced judge exists.
- Judge names are unique.
- Every judge's `provider` is configured and supported.
- Every judge has at least one rubric.
- Every judge's `sampling_rate` is in `[0.0, 1.0]`.

Any violation fails fast with an error message that names the offending agent or judge and field.
