# YAML schema

A complete reference for every field in `coulisse.yaml`.

## IDE autocompletion and validation

Coulisse derives a JSON Schema from the Rust types that parse the YAML, so your editor can autocomplete and lint the config live. Generate the schema next to your config:

```sh
coulisse schema > coulisse.schema.json
```

Then reference it from the top of `coulisse.yaml` with the [yaml-language-server](https://github.com/redhat-developer/yaml-language-server) directive (recognised by the VS Code YAML extension, Helix, Neovim, Zed, JetBrains, etc.):

```yaml
# yaml-language-server: $schema=./coulisse.schema.json
```

The schema is also shipped at the repo root as `coulisse.schema.json` and is the single source of truth for the field tables below — they describe the same shape in prose.

## Environment variables

Any string value in `coulisse.yaml` can reference an environment variable with `${VAR_NAME}`:

```yaml
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
  openai:
    api_key: ${OPENAI_API_KEY}
```

Coulisse expands all `${...}` placeholders before parsing the YAML, so substitution works in any field — API keys, URLs, tokens, passwords, MCP env blocks, etc.

If a referenced variable is not set in the environment, the server refuses to start and prints an error naming the missing variable. An unclosed `${` with no matching `}` is also rejected at startup.

## Config variables

Named text snippets declared under a top-level `vars:` block and spliced into other string fields with `${vars.<name>}`. Useful for sharing a preamble footer across agents, repeating a path, or factoring any string that would otherwise duplicate.

```yaml
vars:
  team_footer: |
    Team: @pm, @coder, @qa
    Rooms: #standup, #engineering, #worklog

agents:
  - name: pm
    provider: anthropic
    model: claude-opus-4-7
    preamble: |
      You are the PM.
      ${vars.team_footer}
  - name: coder
    provider: anthropic
    model: claude-sonnet-4-6
    preamble: |
      You are the coder.
      ${vars.team_footer}
```

`${vars.<name>}` is resolved **after** environment-variable expansion, so a var's value can itself contain `${VAR}` references. Substitution is **single-pass**: a substituted value containing `${vars.x}` is not re-expanded. Unknown `${vars.x}` references abort startup with the offending line.

Multi-line var values inherit the placeholder's leading indent — every line after the first gets prefixed with the same whitespace as the line containing `${vars.x}`. This lets a snippet splice cleanly into a YAML block scalar (`preamble: |`) without breaking the indentation contract.

## Top-level

```yaml
agents: [ ... ]               # required, non-empty
auth: { ... }                 # optional; per-scope auth for /v1/* and /admin/*
default_user_id: <string>     # optional, unset by default
experiments: [ ... ]          # optional; A/B test groups over agents
judges: [ ... ]               # optional; empty/omitted = no evaluation
mcp: { ... }                  # optional
memory: { ... }               # optional; defaults to sqlite history, no long-term memory
providers: { ... }            # required
public_base_url: <string>     # optional; used for MCP OAuth redirect URIs (default: http://localhost:{port})
server: { ... }               # optional; bind/port/threads/body-limit (defaults to 0.0.0.0:8421)
sidecars: [ ... ]             # optional; long-lived helper processes Coulisse spawns alongside itself
skills: { ... }               # optional; skill directory (defaults to ./skills)
smoke_tests: [ ... ]          # optional; synthetic-user evaluation runs
storage: { ... }              # optional; file upload backend (default: fs, no quota)
telemetry: { ... }            # optional; fmt + sqlite on by default, OTLP opt-in
triggers: [ ... ]             # optional; cron / webhook / boot
vars: { name: value, ... }    # optional; named snippets referenced via ${vars.<name>}
```

## `auth`

- **Type:** object
- **Optional.** Omit to leave both surfaces unauthenticated (fine for local dev, never for anything exposed beyond loopback).

Two independent scopes:

- `auth.proxy` guards the OpenAI-compatible `/v1/*` surface that SDK clients call.
- `auth.admin` guards the `/admin/*` surface (the studio UI).

Each scope is itself optional and accepts the same shape: exactly one of `basic`, `oidc`, or `tokens` when present (`tokens` on the `proxy` scope only). They are mutually exclusive within a scope — the server rejects a scope block that has more than one or none. The two scopes are independent, so you can enable Basic on one and OIDC on the other.

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

### `auth.proxy.identity`

How the per-user identity that partitions memory, recall, MCP sessions, and rate limits is derived. Only valid on the `proxy` scope — the `admin` surface has no per-user partitioning, so `from_credential` there is rejected at startup.

| Value             | Behavior |
|-------------------|----------|
| `from_request`    | **Default.** Trust the `safety_identifier` (or deprecated `user`) field in the request body. Correct for single-user setups and trusted first-party backends that set the identifier on behalf of their own authenticated users. |
| `from_credential` | Derive the identity from the authenticated principal — the Basic `username` or the OIDC `sub` claim. A request body claiming a *different* `safety_identifier` is rejected with `403`. Use this for adversarial multi-tenant serving, where clients cannot be trusted to declare their own identity. |

`from_credential` requires `auth.proxy` to declare `basic` or `oidc` (you can't bind to a credential that isn't checked), and is mutually exclusive with [`default_user_id`](#default_user_id) — a shared default bucket would bypass the binding. With Basic, every distinct user needs distinct credentials, since the username *is* the identity; OIDC gives each user a distinct `sub` automatically.

```yaml
auth:
  proxy:
    oidc:
      issuer_url:    https://authentik.example.com/application/o/coulisse/
      client_id:     coulisse-proxy
      client_secret: <secret>
      redirect_url:  http://localhost:8421/v1/
    identity: from_credential   # user = the OIDC subject, not the request body
```

### `auth.proxy.tokens`

Self-issued API tokens — Coulisse mints `sk-coulisse-…` bearer keys, stores only their hash, and gates `/v1/*` on them. Set the (currently empty) block to turn the scheme on; tokens are then created at runtime, never in YAML:

```yaml
auth:
  proxy:
    tokens: {}   # enable bearer-token auth on /v1/*
```

Clients authenticate exactly like the OpenAI API: `Authorization: Bearer sk-coulisse-…`. Each token binds to a **principal** (the user id that partitions memory, recall, and rate limits), so token auth always implies credential-bound identity — a request body claiming a different `safety_identifier` is rejected with `403`, and `default_user_id` does not apply.

Mint, monitor spend on, and revoke tokens from the studio's **Tokens** page or the `coulisse token` CLI. Each token carries a budget — unlimited, a lifetime cap, or a per-calendar-month cap; a request that would exceed it is rejected with `429 insufficient_quota` before any provider call. See [API tokens](../features/api-tokens.md).

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
    api_key: ${ANTHROPIC_API_KEY}
  openai:
    api_key: ${OPENAI_API_KEY}
```

## `mcp`

- **Type:** map of `server_name → server_config`
- **Optional.** Omit if you don't use tools.

Server names are arbitrary — they're what agents refer to under `mcp_tools`.

A server is either **remote** (declare a `url:`) or **local** (declare a
`command:`). The transport is inferred — a `url:` is HTTP, or SSE if the path
contains `/sse`; a `command:` is stdio — but you can pin it with an explicit
`transport:`.

### Common fields

| Field       | Type   | Required | Notes |
|-------------|--------|----------|-------|
| `transport` | enum   | no       | `http`, `sse`, or `stdio`. Inferred from `url`/`command` when omitted; set it to force a transport (e.g. `sse` on a URL without `/sse`). |

### Remote (`url`)

| Field   | Type   | Required | Notes |
|---------|--------|----------|-------|
| `url`   | string | yes      | MCP endpoint. HTTP, or SSE when the path contains `/sse`. |
| `oauth` | varies | no       | Per-user OAuth is **on by default** for URL-based servers (discover mode). Set `false` to disable on a no-auth server, `{ scopes: [...] }` to override scopes, or a full `{ mode: static, ... }` block for providers without Dynamic Client Registration. See [Per-user OAuth for MCP servers](../features/mcp-oauth.md). |

### Local (`command`)

| Field     | Type       | Required | Notes |
|-----------|------------|----------|-------|
| `command` | string     | yes      | Executable to run (stdio transport). |
| `args`    | `list<str>` | no      | Command-line arguments. |
| `env`     | `map<str,str>` | no   | Environment variables for the child. |

### Examples

```yaml
mcp:
  hello:
    command: uvx
    args: [--from, git+https://..., hello-mcp-server]

  calculator:
    url: http://localhost:8080
    oauth: false                 # no-auth server, skip the connect flow

  todoist:
    url: https://ai.todoist.net/mcp   # per-user OAuth implied
```

## `memory`

- **Type:** object
- **Optional.** Omit for defaults: SQLite at `.coulisse/coulisse-memory.db`, history-only (no long-term user state).

See [Memory configuration](../configuration/memory.md) for the full walkthrough and examples.

### Sub-fields

The database always lives at `.coulisse/coulisse-memory.db`; its location is
not configurable. The only sub-field is `user_state`.

| Field                                 | Type           | Required | Default                                |
|---------------------------------------|----------------|----------|----------------------------------------|
| `user_state`                          | bool or object | no       | `false`                                |
| `user_state.embed_with`               | object         | no       | auto-picked from `providers:`          |
| `user_state.learn_from`               | object         | no       | auto-picked from `providers:`          |
| `user_state.dedup_threshold`          | float          | no       | 0.9                                    |
| `user_state.max_facts_per_turn`       | int            | no       | 5                                      |
| `user_state.recall_k`                 | int            | no       | 5                                      |

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
| `max_turns`  | integer               | no       | Maximum tool-calling rounds per turn. Default: `8`. Raise for agents that chain many tool calls (e.g. a coder that reads files, edits, and dispatches to QA in one go). |
| `mcp_tools`  | `list<mcp_tool_access>` | no     | Tools this agent may use. |
| `purpose`    | string                | no       | Tool description when this agent is exposed via another agent's `subagents`. Omit for standalone agents; add a concrete one-line description when this agent is meant to be called as a specialist. |
| `skills`     | `list<string>`        | no       | Names of skills (from the top-level `skills:` directory) this agent may use. Each becomes a tool advertised by its description; calling it returns the skill's instructions. Unknown names are ignored. See [Skills](../features/skills.md). |
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

## `server`

- **Type:** object
- **Optional.** Omit the whole block for the defaults below.
- **Purpose:** how the process binds and listens.

| Field            | Type            | Default     | Purpose                                                                                                  |
| ---------------- | --------------- | ----------- | -------------------------------------------------------------------------------------------------------- |
| `bind`           | string (IP)     | `0.0.0.0`   | Interface to bind. Set `127.0.0.1` to accept loopback only (behind a reverse proxy or tunnel).           |
| `port`           | integer (u16)   | `8421`      | TCP port. Give each `coulisse.yaml` its own port when running multiple instances on one machine.         |
| `worker_threads` | integer         | CPU count   | tokio worker-thread count. Read once at startup; changing it requires a restart.                         |
| `max_body_bytes` | integer         | axum 2 MiB  | Largest accepted request body. Raise for big attachment uploads; lower to harden a public endpoint.      |

```yaml
server:
  bind: 0.0.0.0
  port: 8421
  worker_threads: 4
  max_body_bytes: 8388608   # 8 MiB
```

> The `port` field moved here from the top level in this release. A bare top-level `port:` is no longer read — nest it under `server:`.

## `skills`

- **Type:** object
- **Optional.** Omit the whole block to scan the default `./skills` directory.
- **Purpose:** where reusable skill bundles are loaded from.

| Field | Type   | Default     | Purpose                                                                                    |
| ----- | ------ | ----------- | ------------------------------------------------------------------------------------------ |
| `dir` | string | `./skills`  | Directory holding one subdirectory per skill, each with a `SKILL.md`. A missing directory yields no skills (not an error). |

```yaml
skills:
  dir: ./skills
```

Each subdirectory with a `SKILL.md` becomes a skill; agents opt in by listing skill names under their own `skills:` array. See [Skills](../features/skills.md) for the `SKILL.md` format, bundled resource files, and the `skill_file` tool.

## `smoke_tests`

- **Type:** list of smoke test configs
- **Optional.** Omit (or leave empty) for no synthetic-user runs.

Each entry pairs a *persona* (an LLM that role-plays the user) with a target agent or experiment. Triggered from the studio at `/admin/smoke/<name>`. See [Smoke tests](../features/smoke-tests.md) for the workflow.

### Per-test fields

| Field             | Type              | Required | Default | Notes                                                                       |
|-------------------|-------------------|----------|---------|-----------------------------------------------------------------------------|
| `name`            | string            | yes      | —       | Unique within `smoke_tests`.                                                |
| `target`          | string            | yes      | —       | Agent or experiment name. Resolved per run via the experiment router.       |
| `persona`         | object            | yes      | —       | `provider`, `model`, `preamble` for the role-played user.                   |
| `initial_message` | string            | no       | —       | Hard-coded first persona turn. Omit to let the persona open the conversation. |
| `stop_marker`     | string            | no       | —       | Substring that ends the run when emitted by either side.                    |
| `max_turns`       | integer           | no       | `10`    | Cap on persona-then-agent pairs per run.                                    |
| `repetitions`     | integer           | no       | `1`     | Independent runs launched per click. Each gets a fresh synthetic user id.   |

### Example

```yaml
smoke_tests:
  - name: jobseeker_basic
    target: tremplin
    persona:
      provider: anthropic
      model: claude-haiku-4-5-20251001
      preamble: |
        You are a 28-year-old looking for a developer job in Paris.
        Reply like a real human; finish with "[FIN]" once satisfied.
    initial_message: "Hi, I'm looking for work."
    stop_marker: "[FIN]"
    max_turns: 10
    repetitions: 5
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

- All `${VAR_NAME}` placeholders resolve to set environment variables.
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
