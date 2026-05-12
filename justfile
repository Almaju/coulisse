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
    cargo install cargo-dylint dylint-link --locked
    cargo install cargo-oneway --git https://github.com/Almaju/oneway

lint:
    cargo oneway lint

refresh-prices:
    curl -fsSL \
        https://raw.githubusercontent.com/BerriAI/litellm/main/litellm/model_prices_and_context_window_backup.json \
        -o crates/providers/data/model_prices.json
    @echo "Updated crates/providers/data/model_prices.json"

start:
    ./target/release/coulisse
