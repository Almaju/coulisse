# CLI reference

Coulisse ships as a single binary with a handful of subcommands. Every
subcommand accepts `-c, --config <PATH>` (default `coulisse.yaml`) and
honors the `COULISSE_CONFIG` env var as a fallback.

State files (`coulisse.pid`, `coulisse.log`) live in a `.coulisse/`
directory next to the config file — this keeps state co-located with
the project and makes `cd && coulisse stop` "just work."

## `coulisse init`

Write a starter `coulisse.yaml` in the current directory.

```bash
coulisse init                 # minimal template (one OpenAI agent + sqlite memory)
coulisse init --from-example  # full annotated example (every section, every option)
coulisse init --force         # overwrite an existing coulisse.yaml
```

## `coulisse start`

Start the server, detached by default. Returns once the server has
written its PID file (or fails if the boot times out within 5 seconds).

```bash
coulisse start                # detached background server
coulisse start --foreground   # attached: logs stream to the terminal
coulisse start -F             # short form
```

A bare `coulisse` invocation is equivalent to `coulisse start
--foreground` — the historical pre-subcommand behavior is preserved.

When detached, stdout/stderr are appended to `.coulisse/coulisse.log`.

## `coulisse stop`

Send SIGTERM to a running detached server (PID read from
`.coulisse/coulisse.pid`).

```bash
coulisse stop          # graceful: SIGTERM, wait up to 10s
coulisse stop --force  # SIGKILL (use if the server is wedged)
```

Stop is a no-op if the server isn't running — stale PID files left
over from crashes are detected and removed.

## `coulisse restart`

Equivalent to `coulisse stop && coulisse start`.

## `coulisse status`

Report whether the detached server is running and where its files live.

```text
running (pid 31427)
  config: ./coulisse.yaml
  log:    ./.coulisse/coulisse.log
```

## `coulisse check`

Load and validate the YAML without starting the server. Catches
schema errors and cross-reference issues (agent → provider, agent →
judge, experiment variant → agent, ...) before a real start.

```bash
coulisse check
# ok — coulisse.yaml (3 agents, 1 judges, 0 experiments, 2 providers)
```

## `coulisse update`

Fetch the latest release from GitHub and replace the running binary
in place. Detects the host target triple (e.g.
`aarch64-apple-darwin`) and downloads the matching cargo-dist
artifact. No-op if you're already on the latest version.

```bash
coulisse update
# checking for updates...
# updated to 0.2.0
```

The binary needs write permission to its own path — if you installed
under `/usr/local/bin` you may need `sudo`.

## State directory layout

```text
your-project/
├── coulisse.yaml
└── .coulisse/
    ├── coulisse.pid     # written by `start`, removed on clean exit
    ├── coulisse.log     # detached stdout/stderr
    └── memory.db        # if you point memory.backend.path here
```

`.coulisse/` is the recommended target for `memory.backend.path` so
the whole runtime footprint of one project sits under a single
directory.
