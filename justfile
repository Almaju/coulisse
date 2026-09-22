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

fmt:
    rabot fmt
    cargo fmt --all

install:
    cargo install cargo-watch --locked
    cargo install mdbook --locked
    cargo install cargo-machete --locked
    cargo install --git https://github.com/almaju/rabot --tag v0.1.4 --locked

lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked
    rabot fmt --check
    rabot check --strict
    cargo machete --with-metadata

local:
    cargo install --path cli --bin coulisse --locked

refresh-prices:
    curl -fsSL \
        https://raw.githubusercontent.com/BerriAI/litellm/main/litellm/model_prices_and_context_window_backup.json \
        -o crates/providers/data/model_prices.json
    @echo "Updated crates/providers/data/model_prices.json"

start:
    ./target/release/coulisse
