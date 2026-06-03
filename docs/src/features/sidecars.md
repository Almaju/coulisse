# Sidecars

A sidecar is a long-lived external process Coulisse spawns alongside itself: a Slack listener, a custom metrics exporter, a bridge to whatever chat platform you use — anything you'd otherwise launch in a separate terminal.

The point is *not* to add new agent capabilities — agents already get the world via MCP. The point is to keep "one YAML, one start command" honest. If running Coulisse needs you to remember to also run a bridge script, that property has quietly broken.

Coulisse stays platform-agnostic. The sidecars mechanism only knows how to spawn a command, capture its output, and restart it on crash.

## Declaring sidecars

```yaml
sidecars:
  - name: chat-bridge
    command: chat-bridge/.venv/bin/python
    args: [chat-bridge/bridge.py]
    env:
      BOT_PASSWORD: coulisse-dev
    restart: on-failure

  - name: heartbeat
    command: /bin/sh
    args: ["-c", "while true; do echo alive; sleep 60; done"]
    restart: always
```

Fields:

- **name** — stable identifier; appears in every log line emitted by or about the sidecar. Must be unique.
- **command** — the executable. Absolute path or anything on `PATH`. No shell expansion — quote inside YAML if you need spaces.
- **args** — argv entries, one per list item.
- **env** — environment variables merged on top of Coulisse's own env. `${VAR}` placeholders expand the same way the rest of `coulisse.yaml` expands them, so secrets don't have to be inlined.
- **cwd** — working directory. Defaults to wherever you ran `coulisse start`.
- **restart** — `always` / `on-failure` (default) / `never`. `on-failure` skips a clean exit (`status code 0`); the other two are self-explanatory.

## What happens when a sidecar runs

1. Coulisse spawns the command in a tokio task at startup.
2. The sidecar's stdout and stderr are routed line-by-line into Coulisse's own tracing log, tagged with `sidecar=<name>` and `stream=stdout|stderr`. You'll see them next to MCP messages and request logs.
3. When the process exits, Coulisse evaluates the restart policy and either backs off for two seconds and respawns, or stops watching the sidecar.
4. There's no health check beyond "is the process still alive." If your sidecar hangs without exiting, Coulisse won't notice.

## When *not* to use a sidecar

- If the work is part of the agent flow, expose it as an MCP server instead — that's the abstraction agents actually use.
- If the work is short-lived (a one-shot script), schedule it as a cron trigger that runs a small agent prompt instead.
- If the work needs to outlive Coulisse (database, message broker, homeserver), don't manage it as a sidecar — run it under your real init system (systemd, docker, supervisord). Sidecars die with Coulisse.

## Known limitations

- **Orphan processes on abrupt shutdown.** Tokio's `kill_on_drop` sends `SIGKILL` to children when their `Child` handle drops, but if Coulisse itself is killed before the runtime can run those destructors, children get reparented to PID 1 and keep running. `coulisse stop` is a clean SIGTERM; in practice you may need `pkill -f <command>` to clean orphans up. A graceful-shutdown pass that explicitly SIGTERMs sidecars first is on the roadmap.
- **No retries-with-backoff.** Crash-loop policy is fixed at two seconds. A sidecar that's permanently broken (typo in command, missing dependency) will respawn every two seconds forever.
- **No health checks.** A hung sidecar that doesn't exit looks alive forever.
- **No admin surface.** Sidecar state lives only in the log. A future `/admin/sidecars` page would show running / restart-count / last-output.
