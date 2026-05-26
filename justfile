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

local:
    cargo install --path cli --bin coulisse --locked

matrix-down:
    docker compose -f compose.matrix.yaml down

matrix-init:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p docker/matrix/synapse-data
    if [ ! -f docker/matrix/synapse-data/homeserver.yaml ]; then
        echo "Generating Synapse config..."
        docker run --rm \
            -v "$(pwd)/docker/matrix/synapse-data:/data" \
            -e SYNAPSE_SERVER_NAME=localhost \
            -e SYNAPSE_REPORT_STATS=no \
            matrixdotorg/synapse:latest generate
    fi
    echo "Starting Synapse..."
    docker compose -f compose.matrix.yaml up -d synapse
    echo "Waiting for Synapse to accept connections..."
    until curl -fsS http://localhost:8008/_matrix/client/versions > /dev/null 2>&1; do
        sleep 1
    done
    BOT_PASSWORD="${COULISSE_BOT_PASSWORD:-coulisse-dev}"
    echo "Registering bot user 'coulisse' (idempotent)..."
    docker compose -f compose.matrix.yaml exec -T synapse \
        register_new_matrix_user -u coulisse -p "$BOT_PASSWORD" -a -c /data/homeserver.yaml http://localhost:8008 \
        || echo "(user may already exist — continuing)"
    echo "Starting Element + matrix-mcp..."
    COULISSE_BOT_PASSWORD="$BOT_PASSWORD" docker compose -f compose.matrix.yaml up -d
    echo ""
    echo "Matrix stack ready:"
    echo "  Element Web : http://localhost:8009"
    echo "  Synapse     : http://localhost:8008"
    echo "  matrix-mcp  : http://localhost:7421"
    echo ""
    echo "Bot account  : @coulisse:localhost  (password: $BOT_PASSWORD)"
    echo ""
    echo "Next steps in Element:"
    echo "  1. Register your own user on http://localhost:8008"
    echo "  2. Create rooms: #standup, #product, #engineering, #release"
    echo "  3. Invite @coulisse:localhost to each room"

matrix-logs:
    docker compose -f compose.matrix.yaml logs -f

matrix-up:
    docker compose -f compose.matrix.yaml up -d

refresh-prices:
    curl -fsSL \
        https://raw.githubusercontent.com/BerriAI/litellm/main/litellm/model_prices_and_context_window_backup.json \
        -o crates/providers/data/model_prices.json
    @echo "Updated crates/providers/data/model_prices.json"

start:
    ./target/release/coulisse
