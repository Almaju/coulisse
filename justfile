default:
    @just --list

build:
    cargo build --release

dev:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT
    mdbook serve docs --port 4421 &
    cargo watch -x run &
    wait

install:
    cargo install cargo-watch --locked
    cargo install mdbook --locked
    cargo install cargo-dylint --locked
    cargo install dylint-link --locked

# The rule set is fetched from github.com/Almaju/oneway and built against
# its own rust-toolchain (nightly + rustc-dev). Requires `cargo dylint`
# (and `dylint-link`) — run `just install` first if you haven't.

# Run the oneway-lints dylint rules over the workspace.
lint:
    cargo dylint --all

start:
    ./target/release/coulisse

# Refresh the vendored model pricing snapshot from upstream LiteLLM. Run
# manually when new models ship. The diff lands in git like any other
# code change so updates are reviewable.
refresh-prices:
    curl -fsSL \
        https://raw.githubusercontent.com/BerriAI/litellm/main/litellm/model_prices_and_context_window_backup.json \
        -o crates/providers/data/model_prices.json
    @echo "Updated crates/providers/data/model_prices.json"
