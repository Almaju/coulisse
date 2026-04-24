# Agents

Agents are the named personas clients can talk to. Each agent pins down:

- Which provider to call
- Which upstream model to ask for
- What system prompt to prepend
- Which tools (if any) to expose

## Shape

```yaml
agents:
  - name: code-reviewer
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: |
      You are a thorough code reviewer. Focus on correctness,
      clarity, and security. Point out subtle bugs and suggest
      concrete improvements.
    mcp_tools:
      - server: hello
        only:
          - say_hello
```

## Fields

### `name` (required)

The agent identifier. Clients select this agent by passing `name` as the `model` field in their request. Names must be unique across the config.

### `provider` (required)

Must match a key under the top-level `providers` map. Tells Coulisse which backend to route through.

### `model` (required)

The upstream model identifier. This is provider-specific — e.g. `claude-sonnet-4-5-20250929` for Anthropic, `gpt-4o` for OpenAI, `gemini-2.0-flash` for Gemini.

### `preamble` (optional)

A system prompt prepended to every conversation this agent handles. Use it to define tone, expertise, constraints, output format — anything you'd normally put in a system message.

Defaults to empty. YAML block scalars (`|`) are handy for multi-line preambles.

### `mcp_tools` (optional)

A list of MCP servers and tools this agent is allowed to use. See [MCP tools](./mcp.md) for the full story.

```yaml
mcp_tools:
  - server: hello           # all tools from "hello"
  - server: calculator      # all tools from "calculator"
    only:                   # …but only these specific ones
      - add
      - multiply
```

### `subagents` (optional)

A list of other agent names exposed to this agent as callable tools. When the agent's model decides to invoke one, Coulisse starts a fresh conversation against that agent and returns its final message as the tool result.

```yaml
subagents: [onboarder, resume_critic]
```

Each name must refer to another entry under `agents`. Self-reference and duplicates are rejected at startup. Nested invocations are capped at depth 4 to prevent runaway loops. See [Multi-agent routing](../features/multi-agent.md#subagents-agents-as-tools) for the full walkthrough.

### `purpose` (optional)

A short tool description shown to other agents when this one is listed under their `subagents`. Keep it concrete — it's how a calling agent's model decides when to invoke this specialist. Omit it for agents that are only used directly by clients (never as subagents); fall back is `"Invoke the '<name>' subagent."` but a hand-written `purpose` is what makes multi-agent orchestration reliable.

```yaml
purpose: Critique and rewrite a resume for a target role.
```

## Several agents, one config

Define as many agents as you want. A common pattern is having variants of the same model with different preambles:

```yaml
agents:
  - name: friendly
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: You are warm and encouraging.

  - name: terse
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: Reply in one sentence. No preamble, no filler.

  - name: pirate
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: Respond exclusively as a pirate, arrr.
```

Clients switch between them by changing the `model` field — no server redeploy, no code change.
