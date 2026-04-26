# Installation

Coulisse is a single Rust binary. You build it from source.

## Requirements

- Rust (edition 2024) — install from [rustup.rs](https://rustup.rs)
- A valid API key for at least one supported provider

## Build from source

```bash
git clone <your-coulisse-repo>
cd coulisse
cargo build --release
```

The binary lands at `target/release/coulisse`. Drop it on your `PATH`
(or alias it) so the rest of this guide can call it as `coulisse`.

## Initialize a config

```bash
coulisse init
```

This writes a minimal `coulisse.yaml` in the current directory: one
OpenAI agent, sqlite memory, the offline `hash` embedder. Run
`coulisse init --from-example` instead for the full annotated tour
covering every section.

Edit the file to set your provider API key.

## Start the server

```bash
coulisse start
```

`start` runs the server **detached**: it returns immediately and the
process keeps running in the background. Stop it later with
`coulisse stop`.

To run attached (logs streaming to your terminal), use
`coulisse start --foreground` — or just `coulisse` with no subcommand.
Either form binds **port 8421**.

You should see a startup banner like:

```text
  coulisse 0.1.0

  Proxy   →  http://localhost:8421/v1
  Admin   →  http://localhost:8421/admin

  Memory     sqlite at ./.coulisse/memory.db; embedder=hash (dims=256, OFFLINE — no semantic understanding)
  Auth       proxy: open · admin: open

  Agents (1)
    assistant  openai / gpt-4o-mini
```

The exact lines depend on your config — what matters is that memory,
auth, and every configured agent are each acknowledged on startup.

Next: write [your first config](./first-config.md), or read the
[CLI reference](../reference/cli.md) for every subcommand.
