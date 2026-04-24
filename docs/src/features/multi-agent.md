# Multi-agent routing

Coulisse lets you define multiple agents and route between them with nothing more than the `model` field of a request. No extra endpoints, no custom headers, no proxy tricks.

## Why it matters

Most apps end up needing more than one model configuration:

- A fast, cheap agent for classification and quick replies.
- A heavier agent for hard reasoning.
- A specialized agent (code reviewer, translator, summarizer) with a tuned preamble.
- A tool-using agent that can reach into an MCP server.

Without something like Coulisse, that means either multiple deployments or a growing pile of `if (mode === ...)` switches inside your app.

## The pattern

Declare each variant as a separate agent:

```yaml
agents:
  - name: triage
    provider: anthropic
    model: claude-haiku-4-5-20251001
    preamble: Classify the user's intent. Reply with a single word.

  - name: reasoner
    provider: anthropic
    model: claude-opus-4-7
    preamble: You are a careful reasoner. Think step by step.

  - name: translator
    provider: openai
    model: gpt-4o
    preamble: Translate the user's message into French.
```

Your application picks which agent to call by setting the `model` field:

```python
fast  = client.chat.completions.create(model="triage", ...)
smart = client.chat.completions.create(model="reasoner", ...)
fr    = client.chat.completions.create(model="translator", ...)
```

## What each agent brings to the request

When a request arrives, Coulisse:

1. Looks up the named agent.
2. Prepends the agent's preamble as a system message.
3. Resolves the agent's allowed MCP tools (if any).
4. Forwards the call to the agent's configured provider and model.
5. Records the exchange in the caller's per-user memory.

Changing agents is free — you don't need to redeploy anything on the client side.

## Discovering agents at runtime

`GET /v1/models` returns every agent in the config in OpenAI's standard model-list format. Useful for UIs that want to populate a model picker from the server:

```bash
curl http://localhost:8421/v1/models
```
