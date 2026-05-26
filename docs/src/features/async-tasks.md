# Async tasks

Coulisse's primary surface is the OpenAI-compatible `/v1/chat/completions` endpoint — synchronous, request/response. That's the right shape for chat-driven workflows where a user is waiting on a reply.

It's the wrong shape for everything else: research that takes minutes, scheduled checks, agents that should keep running after the user closes the tab, narration emitted as work progresses. For those, Coulisse has an async lane built on top of the same agent runtime.

## How it works

A `tasks` table stores work the system has accepted but hasn't completed:

```
queued → running → done | errored
```

When something fires off a task — currently the `dispatch_task` tool from inside an agent run, with cron/webhook/MCP-event triggers planned next — a row lands in the table. A background worker pool inside the same Coulisse process drains the queue: each worker pulls the oldest queued task, transitions it to `running`, calls the same `Agents::complete` path the sync HTTP endpoint uses, and writes the final reply (or the error) back to the row.

Workers don't know how their task got enqueued. They just see "run agent X with prompt Y for user Z." That's deliberate — every trigger type produces the same shape of work, so adding new triggers (cron next, then webhooks, then MCP event subscriptions) doesn't touch the worker code.

## Dispatching from an agent

Any agent with a configured task queue gets a built-in `dispatch_task` tool:

```json
{
  "name": "dispatch_task",
  "description": "Enqueue a fire-and-forget background task...",
  "parameters": {
    "type": "object",
    "properties": {
      "agent":  { "type": "string" },
      "prompt": { "type": "string" }
    },
    "required": ["agent", "prompt"]
  }
}
```

The agent calls it with the target agent name and an initial prompt; the tool returns a `task_id` immediately and the worker pool runs it in the background. The dispatching agent gets back only the id — *not* the result. This is the difference from the synchronous subagent dispatch (`subagents: [...]` in YAML), which blocks until the target replies.

When to use which:

- **Subagent dispatch (sync)** — you need the answer before you can continue. *"Ask user-tester for friction analysis, then summarize."*
- **`dispatch_task` (async)** — the work is genuinely fire-and-forget, or it's too long to make the caller wait. *"Start a research task on X. I'll narrate progress as it runs."*

## Configuration

There's no `tasks:` YAML section yet — the queue is always on, with four workers by default. A future `tasks:` block will let you tune worker count and disable the queue entirely if you don't want async work running in your deployment.

## Architecture notes

- Lives in `crates/tasks/`. Owns the `tasks` SQLite table; no other crate touches it.
- The `TaskQueue` trait lives in `coulisse-core` so `agents` can build the `dispatch_task` tool without depending on `tasks` directly. Mirrors the existing `ScoreLookup` / `OneShotPrompt` / `AgentResolver` pattern.
- Workers run in `cli/src/workers.rs`, spawned alongside the HTTP server. They share the same `Agents` runtime — so a background task can call MCP tools, narrate to Matrix, dispatch subagents, exactly like a sync request.
- No special shutdown handling yet. Workers die with the process. A graceful drain that lets in-flight tasks finish before exit is on the roadmap.
