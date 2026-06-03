Configure Coulisse — the local OpenAI-compatible gateway described in `coulisse.yaml`.

$ARGUMENTS

Read the current `coulisse.yaml` first, make the requested change, then validate:

```
coulisse check
```

If the file doesn't exist yet, run `coulisse init` to create a starter template.

## Schema quick reference

### providers (required)

Supported: `anthropic`, `cohere`, `deepseek`, `gemini`, `groq`, `openai`

```yaml
providers:
  openai:
    api_key: ${OPENAI_API_KEY}
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
```

### agents (required, at least one)

The agent `name` is what clients send as the `model` field.

```yaml
agents:
  - name: assistant
    provider: openai
    model: gpt-4o
    preamble: |
      You are a helpful assistant.
    context_limit: 20       # max messages kept in context
    judges: []              # judge names to evaluate responses
    mcp_tools:              # MCP servers + optional tool allowlist
      - server: my_server
        tools: [tool_a, tool_b]   # omit `tools:` to allow all
    subagents: []           # other agent names this one can delegate to
    purpose: …              # short description for subagent routing
```

### memory (optional)

```yaml
memory:
  storage: ./coulisse-memory.db
  user_state: true          # remember facts about users across conversations
```

### mcp (optional)

```yaml
mcp:
  local_tool:
    command: uvx            # stdio: Coulisse spawns the process
    args: [my-mcp-server]
  remote_tool:
    url: https://…/mcp      # HTTP: auto-discovers OAuth
  no_auth_tool:
    url: http://localhost:8080
    oauth: false            # explicit opt-out of OAuth
```

### auth (optional)

```yaml
auth:
  proxy:
    tokens: {}              # enable sk-coulisse-… bearer tokens
  admin:
    basic:
      username: admin
      password: ${ADMIN_PASSWORD}
```

### server (optional)

```yaml
server:
  bind: 0.0.0.0
  port: 8421
  worker_threads: 4
  max_body_bytes: 8388608   # 8 MiB
```

### judges (optional)

```yaml
judges:
  - name: quality
    provider: openai
    model: gpt-4o-mini
    sampling_rate: 1.0
    rubrics:
      helpfulness: Whether the assistant answered the user's question.
      tone: Politeness and professionalism.
```

### experiments (optional)

```yaml
experiments:
  - name: gpt-vs-claude     # clients use this as the model name
    strategy: split         # split | shadow | bandit
    variants:
      - agent: gpt-agent
        weight: 0.5
      - agent: claude-agent
        weight: 0.5
```

### triggers (optional)

```yaml
triggers:
  - name: daily-digest
    agent: assistant
    prompt: Summarize today's highlights.
    kind:
      cron: "0 9 * * *"
  - name: webhook-handler
    agent: assistant
    prompt: "Process this event: {{body}}"
    kind:
      webhook:
        path: /hooks/my-event
```

### vars (optional)

Named snippets you can splice into any string field with `${vars.name}`.

```yaml
vars:
  team_footer: "Team: @alice, @bob"
# Use anywhere: preamble: "… ${vars.team_footer}"
```

### sidecars (optional)

```yaml
sidecars:
  - name: bridge
    command: python
    args: [bridge.py]
    restart: on_failure     # always | on_failure | never
```

## Tips

- Run `coulisse schema > coulisse.schema.json` and add
  `# yaml-language-server: $schema=./coulisse.schema.json` at the top of
  `coulisse.yaml` for IDE autocomplete and inline validation.
- Env vars expand anywhere: `${MY_VAR}`. Named vars (`${vars.x}`) are
  resolved after env-var expansion.
- Agent and experiment names share a namespace — they must be unique across
  both lists.
- `coulisse check` validates the full graph (agent → provider, agent → judge,
  subagent references) without starting the server.
