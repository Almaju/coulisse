# Daily briefing (cron)

**What you get:** an agent that wakes up every morning at 09:00 and does something — summarises, checks in, posts a report — without any user action.

## The config

```yaml
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}

agents:
  - name: briefing
    provider: anthropic
    model: claude-haiku-4-5-20251001
    preamble: |
      You produce a short daily briefing. Be concise — 5 bullet points max.
      Focus on what's actionable today.

triggers:
  - name: morning-briefing
    type: cron
    schedule: "0 9 * * *"     # every day at 09:00
    agent: briefing
    prompt: |
      It's the morning briefing. Summarise what the team should focus on today.
      Check any pending async tasks and flag blockers.
```

## What happens

At 09:00 every day, Coulisse:

1. Fires the `morning-briefing` trigger.
2. Enqueues a task for the `briefing` agent with the configured prompt.
3. The agent runs just like any other request — full preamble, MCP tools if configured, memory if enabled.

The task and its output appear in [Studio UI](../features/studio-ui.md) at `/admin/live`. If the agent has access to MCP tools (e.g. a calendar tool, a task tracker), it can pull live data into its reply.

## Wake-up on boot

For a "check in when the server restarts" variant:

```yaml
triggers:
  - name: wakeup
    type: boot
    agent: briefing
    prompt: |
      You just came back online. Check async task status and
      decide whether anything urgent needs attention. Silence is fine.
```

This fires exactly once per `coulisse start`, after the worker pool is up.

## Combine cron and boot

```yaml
triggers:
  - name: wakeup
    type: boot
    agent: briefing
    prompt: "Just came back online. Anything to catch up on?"

  - name: morning-briefing
    type: cron
    schedule: "0 9 * * *"
    agent: briefing
    prompt: "Morning briefing — summarise priorities for today."

  - name: eod-summary
    type: cron
    schedule: "0 17 * * 1-5"   # weekdays at 17:00
    agent: briefing
    prompt: "End-of-day summary — what was completed, what's pending?"
```

Three independent triggers, same agent, zero extra infrastructure.

## Notes

- Cron expressions follow standard 5-field POSIX syntax (`min hour day month weekday`). Bad expressions are rejected at startup.
- The trigger runs as `default_user_id` (or a synthetic `cron` user). It shares a memory bucket with any human using the same user id — useful if you want the briefing agent to remember context across human and automated sessions.
- Output only appears in the Studio today. If you want it posted somewhere (Slack, Matrix, email), pair it with an MCP tool or a sidecar that reads the task output.

**Next:** [Orchestrator + specialists](./orchestrator-specialists.md)
