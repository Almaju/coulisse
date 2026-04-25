default:
    @just --list

build:
    cd crates/studio && trunk build --release
    cargo build --release

dev:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT
    mdbook serve docs --port 4421 &
    cargo watch -x run &
    # Gate trunk on the server binding :8421 so its proxy doesn't fire at a
    # dead socket during cold compile (or while the server blocks on OAuth).
    until nc -z 127.0.0.1 8421 2>/dev/null; do sleep 1; done
    (cd crates/studio && trunk serve) &
    wait

install:
    rustup target add wasm32-unknown-unknown
    cargo install cargo-watch --locked
    cargo install mdbook --locked
    cargo install trunk --locked

start:
    ./target/release/coulisse
