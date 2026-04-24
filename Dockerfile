# syntax=docker/dockerfile:1

# --- Build stage ---------------------------------------------------------
FROM rust:1-slim AS builder

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml ./
COPY crates ./crates

RUN cargo build --release --bin coulisse

# --- Runtime stage -------------------------------------------------------
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --system --user-group --home-dir /var/lib/coulisse coulisse \
 && mkdir -p /var/lib/coulisse /etc/coulisse \
 && chown -R coulisse:coulisse /var/lib/coulisse /etc/coulisse

COPY --from=builder /build/target/release/coulisse /usr/local/bin/coulisse

USER coulisse
WORKDIR /var/lib/coulisse

# Memory database lives here — mount a volume or bind-mount a host path to
# persist across container restarts.
VOLUME ["/var/lib/coulisse"]

# YAML config is expected at /etc/coulisse/coulisse.yaml. Mount your own
# file there at runtime.
ENV COULISSE_CONFIG=/etc/coulisse/coulisse.yaml

EXPOSE 8421

CMD ["coulisse"]
