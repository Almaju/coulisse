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

start:
    ./target/release/coulisse
