# Orchestrator + specialists

**What you get:** one agent that handles the conversation and delegates focused sub-tasks to specialist agents — without the user knowing multiple models are involved.

## The config

```yaml
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}

agents:
  - name: onboarder
    provider: anthropic
    model: claude-haiku-4-5-20251001
    purpose: Collect missing user profile fields (name, role, goals). Ask one question at a time.
    preamble: |
      Your only job is to collect user profile information.
      Ask for the first missing field. Be friendly and brief.

  - name: writer
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    purpose: Draft or rewrite text based on a brief and constraints.
    preamble: |
      You are a skilled writer. When given a brief, produce clean, 
      well-structured text. Ask clarifying questions only if the brief 
      is genuinely ambiguous.

  - name: reviewer
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    purpose: Review a piece of text and return structured feedback (strengths, gaps, suggestions).
    preamble: |
      You review text critically. Return your feedback as:
      - Strengths (2-3 points)
      - Gaps (what's missing or unclear)
      - Suggestions (concrete rewrites or additions)

  - name: coach
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    subagents: [onboarder, writer, reviewer]
    preamble: |
      You are a writing coach. Guide the user through their writing project.
      If their profile is incomplete, delegate to onboarder first.
      Delegate drafting to writer and critique to reviewer.
      Synthesise their output and keep the conversation moving.
```

## What happens

The user talks to `coach`. When the coach's LLM decides a sub-task fits a specialist, Coulisse runs that specialist as a tool call:

1. Coach receives: *"I need to write a proposal for a new team process."*
2. If the profile is incomplete: coach calls `onboarder` → gets missing fields → resumes.
3. Coach calls `writer` with a brief → gets a draft → presents it.
4. User asks for feedback → coach calls `reviewer` → presents structured critique.

Each specialist runs with its own preamble and context. The user sees one continuous conversation.

## Try it

```bash
curl http://localhost:8421/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coach",
    "messages": [
      {"role": "user", "content": "I want to propose a new code review process for my team."}
    ]
  }'
```

## Why `purpose` matters

The `purpose` field is the tool description shown to the coach's LLM. A precise `purpose` is what makes routing reliable:

```yaml
# Good
purpose: Collect missing user profile fields (name, role, goals). Ask one question at a time.

# Too vague — the coach won't know when to reach for this
purpose: Helpful assistant
```

Without `purpose`, Coulisse falls back to `"Invoke the '<name>' subagent."` — functional, but blunt.

## Notes

- Each subagent invocation starts a fresh conversation. Specialists don't see the parent's history or each other's output. If you need data to flow, pass it explicitly in the prompt you give the subagent, or store it via an MCP tool that both agents can read.
- Nesting works: a subagent can itself have `subagents`. Coulisse caps depth at 4 to prevent runaway loops.
- This pattern scales: add a `researcher` agent with web-search MCP tools, a `translator`, a `data-analyst` — just list them in `subagents:` on the coach.

**Back to:** [Use cases overview](./README.md)
