# Providers

Providers are where your model calls actually go. Configure each provider once with its credentials; reference it by name from any number of agents.

## Supported providers

| Kind        | Config key    |
|-------------|---------------|
| Anthropic   | `anthropic`   |
| Cohere      | `cohere`      |
| Deepseek    | `deepseek`    |
| Gemini      | `gemini`      |
| Groq        | `groq`        |
| OpenAI      | `openai`      |

## Shape

```yaml
providers:
  anthropic:
    api_key: sk-ant-...
  openai:
    api_key: sk-...
  gemini:
    api_key: ...
```

Each provider takes a single field: `api_key`. You only need to list the providers you plan to use — unused ones can be omitted entirely.

## Validation

When Coulisse loads your config, it checks that every agent's `provider` field matches a key under `providers`. Misspell a provider and startup fails with a clear error:

```text
agent 'assistant' references provider 'antropic' which is not configured
```

## Switching providers

Because providers are referenced by name, switching an agent from one backend to another is a one-line change:

```yaml
agents:
  - name: assistant
    provider: anthropic            # ← change this …
    model: claude-sonnet-4-5-20250929   # ← … and this
    preamble: You are helpful.
```

No client code changes, no redeployment of downstream apps. See [Multi-backend support](../features/backends.md) for more on mixing providers.
