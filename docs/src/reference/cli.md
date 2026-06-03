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

## `coulisse reset`

Delete the SQLite database, wiping **all** stored state — conversation
memory, long-term memories, telemetry, judge scores, rate-limit windows,
background tasks, and API tokens. Your `coulisse.yaml` is never touched.

Destructive and irreversible, so it refuses to run while a server holds the
database open (stop it first), and prompts for confirmation unless `-y` is
passed. Removes the database file (`.coulisse/coulisse-memory.db`) plus its
`-wal`/`-shm` sidecars.

```bash
coulisse reset       # warns, lists the files, asks to confirm
coulisse reset -y    # skip the prompt (for scripts / fresh starts)
```

## `coulisse status`

Report whether the detached server is running and where its files live.

```text
running (pid 31427)
  config: ./coulisse.yaml
  log:    ./.coulisse/coulisse.log
```

## `coulisse studio`

Open the studio UI (`/admin/`) in the default web browser. Requires
the server to be running — start it first with `coulisse start`.

```bash
coulisse studio   # also: coulisse admin
# opening http://localhost:8421/admin/
```

The URL honors `server.port` from `coulisse.yaml`, so multiple Coulisse
instances on different ports each open their own studio.

## `coulisse token`

Mint, list, and revoke the self-issued API tokens that gate `/v1/*` when
[`auth.proxy.tokens`](../features/api-tokens.md) is enabled. Operates on
the same database the running server uses, so changes are live immediately.

```bash
coulisse token create laptop --principal alice         # unlimited
coulisse token create ci --principal alice \
  --budget monthly --limit 20                          # $20 / month cap
coulisse token list                                    # tokens + spend
coulisse token revoke <id>                             # immediate 401 for clients
```

`create` prints the secret (`sk-coulisse-…`) to stdout — shown only once —
and the id/context to stderr, so `coulisse token create … > key.txt`
captures just the key.

## `coulisse check`

Load and validate the YAML without starting the server. Catches
schema errors and cross-reference issues (agent → provider, agent →
judge, experiment variant → agent, ...) before a real start.

```bash
coulisse check
# ok — coulisse.yaml (3 agents, 1 judges, 0 experiments, 2 providers)
```

## `coulisse schema`

Emit the JSON Schema for `coulisse.yaml` to stdout. Redirect to a file
next to your config and reference it for IDE autocompletion and
validation:

```bash
coulisse schema > coulisse.schema.json
```

```yaml
# yaml-language-server: $schema=./coulisse.schema.json
```

Picked up by the VS Code YAML extension, Helix, Neovim, Zed, JetBrains —
anything that speaks the yaml-language-server directive. The schema is
generated from the same Rust types that parse the config, so it never
drifts.

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
    ├── coulisse.pid          # written by `start`, removed on clean exit
    ├── coulisse.log          # detached stdout/stderr
    ├── secrets.env           # MCP OAuth encryption keys (when configured)
    ├── files/                # uploaded file blobs (fs storage backend)
    └── coulisse-memory.db    # SQLite database
```

`.coulisse/` holds the whole runtime footprint of one project under a
single directory: the SQLite database, uploaded files, logs, PID, and
secrets all land here, and the paths are not configurable. Mount this one
directory to persist Coulisse's state in Docker.
