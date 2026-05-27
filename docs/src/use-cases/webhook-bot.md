# Slack / webhook bot

**What you get:** any external system that can POST JSON fires an agent. No Slack SDK in Coulisse — just an HTTP endpoint your webhook points at.

This example uses Slack, but the same pattern works for GitHub webhooks, Stripe events, or anything else that speaks HTTP.

## The config

```yaml
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}

agents:
  - name: slack-bot
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: |
      You are a helpful Slack bot for the engineering team.
      Be concise. Use Slack markdown (bold: *text*, code: `code`).

triggers:
  - name: slack-message
    type: webhook
    path: /hooks/slack
    agent: "slack-bot"
    prompt: "Message from @{{user_name}} in #{{channel_name}}: {{text}}"
```

## What happens

1. Someone sends a message in Slack.
2. Slack's outgoing webhook POSTs to `http://your-server:8421/hooks/slack` with a JSON body that includes `user_name`, `channel_name`, and `text`.
3. Coulisse renders the prompt template with those values and enqueues a task.
4. The `slack-bot` agent runs, and the response appears in `/admin/live`.

Coulisse returns `{ "ok": true, "task_id": "..." }` immediately — the agent runs in the background.

## Fire it manually

```bash
curl -X POST http://localhost:8421/hooks/slack \
  -H "Content-Type: application/json" \
  -d '{
    "user_name": "alice",
    "channel_name": "engineering",
    "text": "What does our rate limiting look like?"
  }'
```

Watch the task run in [Studio UI](../features/studio-ui.md) at `http://localhost:8421/admin/live`.

## Routing to different agents by payload

If you want different agents to handle different event types:

```yaml
triggers:
  - name: slack-mention
    type: webhook
    path: /hooks/slack
    agent: "{{target_agent}}"
    prompt: "@{{user_name}}: {{text}}"
```

Your Slack bridge then decides which agent to call and sets `target_agent` in the payload before POSTing.

## Notes

- The webhook path must start with `/hooks/` — this keeps it separate from the OpenAI-compatible proxy (`/v1/*`) and the Studio (`/admin/*`).
- Coulisse doesn't verify webhook signatures today. For Internet-facing deployments, put Coulisse behind a reverse proxy that validates signatures before forwarding, or restrict the `/hooks/*` path to trusted IPs.
- The task response isn't automatically sent back to Slack. You need a bridge that reads the completed task (via `/admin/live` polling or a follow-up call) and posts the result. A simple reference bridge pattern is described in [Matrix as the chat UI](../features/matrix-chat.md) — the shape is the same for Slack.

**Next:** [Daily briefing (cron)](./daily-briefing.md)
