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

The binary lands at `target/release/coulisse`.

## Run

Coulisse reads `coulisse.yaml` from the current working directory. Copy the example to start:

```bash
cp coulisse.example.yaml coulisse.yaml
```

Edit `coulisse.yaml`, drop in an API key, then start the server:

```bash
./target/release/coulisse
```

You should see output like:

```text
coulisse listening on http://0.0.0.0:8421
  memory: HashEmbedder (MVP placeholder — swap for a real embedder before production)
  agent: claude-assistant (provider=anthropic, model=claude-sonnet-4-5-20250929)
  agent: code-reviewer    (provider=anthropic, model=claude-sonnet-4-5-20250929)
```

The server binds to **port 8421**.

Next: write [your first config](./first-config.md).
