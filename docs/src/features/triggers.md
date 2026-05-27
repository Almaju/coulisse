# Triggers

A trigger is a way to start an agent without anyone making an HTTP request. Cron fires on a schedule; webhooks fire on an inbound POST; boot triggers fire once when Coulisse starts. All three convert to the same shape — a task enqueued via the queue — so the agent runtime doesn't know or care how it was summoned.

This is the primitive that makes Coulisse feel like an office instead of a request handler: agents wake up because *something happened*, not because someone is waiting.

## Why this is platform-agnostic

There's no Matrix-, Slack-, or Discord-specific code in Coulisse. The `webhook` trigger (coming next) accepts JSON POSTs from anything that can speak HTTP. Connecting Matrix means running a tiny standalone bridge that listens on Matrix and POSTs to Coulisse. Connecting Slack means pointing Slack's built-in outgoing webhooks at Coulisse. Connecting GitHub means setting up a webhook on the repo. Coulisse doesn't know the source — it sees an HTTP request.

The cron trigger is purely internal — zero external dependencies.

## Cron triggers

Configure under the top-level `triggers:` list in `coulisse.yaml`:

```yaml
triggers:
  - name: daily-standup
    type: cron
    schedule: "0 9 * * *"      # every day at 09:00
    agent: pm
    prompt: "Standup matin — résume l'activité d'hier en 5 puces."

  - name: hourly-watch
    type: cron
    schedule: "0 * * * *"       # every hour at :00
    agent: user-tester
    prompt: "Une phrase sur le ressenti du moment."
```

Fields:

- **name** — stable identifier used in logs and admin views. Must be unique within the file.
- **type: cron** — the discriminator. Other types (`webhook`) arrive later.
- **schedule** — POSIX cron expression. Either 5-field (`min hour day-of-month month day-of-week`) or 6-field with leading seconds (`sec min …`). The 5-field form is normalised to 6-field with a leading `0` seconds. Schedules are validated at startup; bad expressions refuse to boot.
- **agent** — name of the agent (or experiment) to invoke. Must exist in `agents:` / `experiments:`.
- **prompt** — static user message passed to the agent on each fire. Templating from trigger payload arrives with the webhook trigger.

When the trigger fires, Coulisse enqueues a task and a worker runs the agent through the same handler the sync `/v1/chat/completions` endpoint uses. The agent gets its full preamble, MCP tools, subagent dispatch, and narration — nothing about background runs is different. Watch them in `/admin/live`.

## User identity

Cron-triggered tasks run as `default_user_id` (from the top of `coulisse.yaml`). If unset, they run as a synthetic `cron` user. Memory partitions are honoured: if `daily-standup` calls `pm` with `default_user_id: main`, it sees the same memory bucket as a human who sends a chat request as `main`.

## Watching cron fire

Tail the log; you'll see one line per arm and one per fire:

```text
INFO cron trigger armed   trigger=daily-standup agent=pm
INFO cron trigger fired   trigger=daily-standup agent=pm task_id=…
```

Or open `/admin/live` — tasks created by triggers appear in the Tasks panel the same way `dispatch_task` tasks do, with the trigger's prompt as the initial message and the agent name as written in YAML.

## Boot triggers

A `type: boot` trigger fires exactly once when Coulisse starts. Use it for "wake up and decide what to do" prompts that should run on every `coulisse start` — e.g. asking an orchestrator agent to read the queue's leftovers and decide whether a standup is warranted, without forcing a ritual on every restart.

```yaml
triggers:
  - name: wakeup
    type: boot
    agent: pm
    prompt: |
      You just came back online. Check `tasks_status` for what was running
      before the stop, look at recent commits, and decide whether to post
      a standup. Silence is fine when nothing demands attention.
```

Fields:

- **type: boot** — discriminator.
- **agent**, **prompt** — same as cron: which agent runs, with what initial message.

The task is enqueued during `coulisse start`, after the worker pool is up. Combined with the boot-time reaper that marks orphaned `running` tasks as `errored`, this gives the wake-up agent everything it needs to assess state and resume work — see [Async tasks](./async-tasks.md) for the queue semantics.

## Webhook triggers

A `type: webhook` trigger declares an HTTP path; Coulisse exposes `POST <path>` and fires the trigger on each request. This is the universal connector for outside systems — anything that can POST JSON can summon an agent. No Matrix / Slack / Discord code in Coulisse.

```yaml
triggers:
  - name: matrix-mention
    type: webhook
    path: /hooks/matrix-mention      # must start with /hooks/
    agent: pm
    prompt: "Message Matrix de {{sender}} dans {{room_name}} : {{body}}"
```

Fields beyond the cron shape:

- **type: webhook** — discriminator.
- **path** — HTTP path Coulisse exposes. Must start with `/hooks/` to stay clear of the proxy (`/v1/*`), studio (`/admin/*`), and OAuth callbacks (`/mcp/*`). Must be unique across all webhook triggers.
- **agent** — name of the agent (or experiment) to invoke. Accepts the same `{{a.b.c}}` templating as `prompt`, so one webhook can route to different agents based on the inbound payload (see [Templated agent](#templated-agent) below).
- **prompt** — template. `{{a.b.c}}` placeholders pull values from the JSON payload by dot-path. Missing paths render as the literal `{{ a.b.c }}` so debugging is obvious. Static prompts (no placeholders) work too — they pass through unchanged.

Fire it with curl:

```bash
curl -X POST http://localhost:8421/hooks/matrix-mention \
  -H 'Content-Type: application/json' \
  -d '{"sender":"@alice:localhost","room_name":"engineering","body":"@coulisse what is the state of the build?"}'
```

Response:

```json
{ "ok": true, "task_id": "cb9b91c4-54db-4b8c-a564-08282e643c25" }
```

The task appears in `/admin/live` like any other.

### Templated agent

The `agent` field accepts the same `{{a.b.c}}` templating as `prompt`. This lets one webhook fan out to different agents based on whatever the inbound payload carries — useful when a bridge POSTs one event per mentioned agent:

```yaml
triggers:
  - name: matrix-mention
    type: webhook
    path: /hooks/matrix-mention
    agent: "{{agent}}"
    prompt: "@{{sender}} in #{{room}}: {{body}}"
```

The bridge does the iteration on its side and calls the same webhook N times, once per mentioned agent:

```bash
curl -X POST http://localhost:8421/hooks/matrix-mention \
  -d '{"agent":"pm","sender":"almaju","room":"standup","body":"any release blockers?"}'

curl -X POST http://localhost:8421/hooks/matrix-mention \
  -d '{"agent":"coder","sender":"almaju","room":"standup","body":"any release blockers?"}'
```

Two tasks land on the queue, one per agent.

A templated `agent` field is **not** cross-validated at config load — the value isn't known until a request arrives. If the resolved name doesn't match any agent, the worker errors the task with an "unknown agent" message; you'll see it in `/admin/live`. If the placeholder fails to resolve at all (the path is missing from the payload), the webhook returns `400 Bad Request` and nothing is enqueued.

### Worked example: Matrix mentions

The reference bridge at `local/matrix-bridge/bridge.py` (gitignored personal workspace) talks Matrix and translates `@<agent>` mentions into calls against Coulisse's OpenAI endpoint. Run as a sidecar — see [Matrix as the chat UI](./matrix-chat.md). Note: today's bridge calls `/v1/chat/completions` *directly*, not a webhook trigger; webhook triggers are still the right path for non-chat sources like GitHub, Slack outgoing webhooks, etc.

## What's not here yet

- **Per-trigger `user_id`.** Today every trigger fires as the same `default_user_id`. A future field will let triggers run as different synthetic users, useful for partitioning memory between scheduled jobs.
- **Skip-on-overlap.** If a cron fires while the previous run is still going, both queue up. A `skip_if_running: true` field would let users opt into "only one at a time."
- **Signature verification on webhooks.** Anyone who can reach `/hooks/<path>` can fire the trigger. For Internet-facing deployments you'd want a shared secret or HMAC check, configurable per trigger. Today the assumption is loopback or trusted network.
