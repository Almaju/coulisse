# Your first config

A minimal `coulisse.yaml` has two things: a **provider** (where to send model calls) and an **agent** (how to call it).

```yaml
providers:
  anthropic:
    api_key: sk-ant-your-key-here

agents:
  - name: assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: You are a helpful assistant.
```

Save this as `coulisse.yaml` in your working directory, then run `coulisse`.

## What each piece does

### `providers`

A map of provider kind → credentials. The key must be one of the supported kinds (see [Providers](../configuration/providers.md)). You only need to list the providers you actually use.

### `agents`

A list of agents. Each agent is a named recipe:

- `name` — the identifier. Clients ask for the agent by this name via the `model` field in their request.
- `provider` — which configured provider to route to.
- `model` — the upstream model identifier to call (e.g. `claude-sonnet-4-5-20250929`, `gpt-4o`).
- `preamble` — optional system prompt prepended to every conversation.

You can define as many agents as you want — see [Multi-agent routing](../features/multi-agent.md) for what that unlocks.

## Adding more

Want a code reviewer, a pirate, and a tool-using agent? Just add more entries:

```yaml
providers:
  anthropic:
    api_key: sk-ant-...
  openai:
    api_key: sk-...

agents:
  - name: assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: You are a helpful assistant.

  - name: code-reviewer
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: |
      You are a thorough code reviewer. Focus on correctness,
      clarity, and security.

  - name: gpt-assistant
    provider: openai
    model: gpt-4o
    preamble: You are a helpful assistant.
```

Restart the server — all three agents are now selectable by model name.

Next: [make a request](./first-request.md).
