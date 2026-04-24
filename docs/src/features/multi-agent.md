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

## Subagents: agents as tools

Routing by `model` lets the client pick an agent per request. Sometimes you want one agent to call another *from within a turn*, so the conversation stays with the top-level agent while specialists handle focused sub-tasks. Coulisse exposes this via the `subagents` field.

```yaml
agents:
  - name: onboarder
    provider: anthropic
    model: claude-haiku-4-5-20251001
    purpose: Collect the user's profile — first name, last name, phone, goals.
    preamble: |
      Ask the user for any missing profile field. Keep questions short.

  - name: resume_critic
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    purpose: Critique and rewrite a resume for a target role.
    preamble: |
      Given a resume and a target role, return a revised resume and
      a bullet list of the biggest gaps to address.

  - name: career_coach
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    subagents: [onboarder, resume_critic]
    preamble: |
      Guide the user. Delegate to `onboarder` if the profile is
      incomplete, and `resume_critic` when they want resume work.
```

When `career_coach` runs, the `onboarder` and `resume_critic` agents appear in its tool list alongside any MCP tools. If the model calls `onboarder`, Coulisse starts a fresh conversation against that agent with just the message it was given — the onboarder sees its own preamble and its own MCP tools, nothing inherited from the parent. The onboarder's final assistant message is returned to the coach as the tool result.

### The `purpose` field

`purpose` is the tool description shown to the calling agent. It's how the coach's LLM decides whether this subagent is the right choice for the current turn. Keep it short and concrete — `"Critique and rewrite a resume for a target role"` is good; `"Helpful assistant"` is useless.

If `purpose` is absent, Coulisse falls back to `"Invoke the '<name>' subagent."` — functional, but a clear `purpose` is what makes orchestration reliable.

### Bounded recursion

Calling a subagent is itself a tool call — the subagent can have its own `subagents`, which can have their own, and so on. To prevent a pathological `A → B → A → …` loop from burning tokens, Coulisse caps nested invocations at depth 4. Going over returns a clear error that the parent agent sees and can react to.

### Fresh context

Every subagent invocation starts with a new conversation. The subagent does **not** see the parent's message history, the user's original request, or any other sibling subagent's output. It gets only the message the parent passed when calling it, plus its own preamble.

This isolation is deliberate. It keeps subagents focused, prevents context bloat, and makes each subagent's behavior reproducible in isolation. If you want data to flow between agents, store it in an MCP server and have both agents read it — Coulisse owns no cross-agent state.

### Why subagents and MCPs live side by side

`mcp_tools` and `subagents` both appear in an agent's tool list, but they model different things:

- An **MCP tool** is a stateless function call against an external server: fixed schema, data in and data out.
- A **subagent** is another LLM conversation that happens to be kicked off by a tool call. It has its own preamble, its own tool loop, and can itself delegate further.

Reach for `mcp_tools` when the work is a concrete operation (save a record, search a database, send an email). Reach for `subagents` when the work needs its own LLM reasoning under a different preamble.
