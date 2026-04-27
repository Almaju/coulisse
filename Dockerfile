# syntax=docker/dockerfile:1

# --- Build stage ---------------------------------------------------------
# Alpine uses musl libc, producing a statically-linked binary with no
# glibc dependency — required for aarch64 hosts (Raspberry Pi) that run
# an older glibc than whatever rust:slim currently ships.
FROM rust:alpine AS builder

RUN apk add --no-cache \
    musl-dev \
    pkgconfig \
    openssl-dev \
    openssl-libs-static

ENV OPENSSL_STATIC=1

WORKDIR /build
COPY Cargo.toml ./
COPY cli ./cli
COPY coulisse.example.yaml ./
COPY crates ./crates

RUN cargo build --release --bin coulisse

# --- Runtime stage -------------------------------------------------------
FROM alpine:3

RUN apk add --no-cache ca-certificates \
 && addgroup -S coulisse \
 && adduser -S -G coulisse -h /var/lib/coulisse coulisse \
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
