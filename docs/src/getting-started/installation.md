# Installation

Coulisse is a single Rust binary. Install it from a prebuilt release or build
from source.

## Requirements

- A valid API key for at least one supported provider

## Install from a release

The latest GitHub Release ships installers for macOS (x86 + ARM), Linux GNU
(x86 + ARM), and Windows MSVC.

**macOS / Linux:**

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Almaju/coulisse/releases/latest/download/coulisse-installer.sh | sh
```

**Windows (PowerShell):**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/Almaju/coulisse/releases/latest/download/coulisse-installer.ps1 | iex"
```

The installer drops the `coulisse` binary on your `PATH`.

## Build from source

Requires Rust (edition 2024) — install from [rustup.rs](https://rustup.rs).

```bash
git clone https://github.com/Almaju/coulisse.git
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
