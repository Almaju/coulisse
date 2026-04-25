# Telemetry

The `telemetry:` block controls observability — what Coulisse logs to stderr, what it persists to SQLite for the studio UI, and whether it ships traces to your own OpenTelemetry backend.

Every field has a sensible default. Omit the block and you get stderr logs at `info` plus the studio's per-turn event tree, with no external traces.

## Shape

```yaml
telemetry:
  fmt:
    enabled: true        # stderr logs; default on
  sqlite:
    enabled: true        # mirrors spans into the studio's tables; default on
  otlp:                  # absent = disabled (default)
    endpoint: "http://localhost:4317"
    protocol: grpc       # or http_binary
    service_name: coulisse
    headers:
      authorization: "Bearer ${OTEL_API_KEY}"
```

All three layers compose. Turn `sqlite` off if you don't need the studio. Add `otlp` to ship the same traces to Grafana, SigNoz, Jaeger, Honeycomb, or any OTLP-compatible backend.

## `telemetry.fmt`

| Field     | Type | Required | Notes |
|-----------|------|----------|-------|
| `enabled` | bool | no       | Default `true`. |

Writes structured logs to stderr. The level is controlled by the `RUST_LOG` environment variable; without it, the default is `info,sqlx=warn` (info from Coulisse, warnings only from the SQL driver). To see internal SQL traffic, run with `RUST_LOG=debug`. To silence everything, set `RUST_LOG=error`.

## `telemetry.sqlite`

| Field     | Type | Required | Notes |
|-----------|------|----------|-------|
| `enabled` | bool | no       | Default `true`. |

Mirrors `turn` and `tool_call` tracing spans into the `events` and `tool_calls` tables that the studio UI reads. Without this layer, the studio loses its per-turn event tree and tool-call panel.

The schema is part of the same SQLite file the rest of Coulisse persists into (controlled by `memory.backend.path`).

## `telemetry.otlp`

Absent (the default) means Coulisse does not export traces externally. To plug Coulisse into your own observability stack, set the block:

| Field          | Type   | Required | Notes |
|----------------|--------|----------|-------|
| `endpoint`     | string | yes      | Collector URL. |
| `protocol`     | enum   | no       | `grpc` (default) or `http_binary`. |
| `service_name` | string | no       | OpenTelemetry resource attribute `service.name`. Default `coulisse`. |
| `headers`      | map    | no       | Static HTTP/gRPC headers attached to every export. |

### Endpoint defaults

- **gRPC** (the default): port `4317`, e.g. `http://localhost:4317`.
- **HTTP/protobuf**: port `4318`, e.g. `http://localhost:4318/v1/traces`.

The collector you point at decides the rest — Coulisse ships *traces* with `service.name = coulisse` and span names `turn`, `tool_call`, and `llm_call`. Span fields carry `user_id`, `turn_id`, `agent`, `tool_name`, `kind`, and the rest documented in the [features chapter](../features/telemetry.md).

### Headers

Useful for managed backends:

```yaml
telemetry:
  otlp:
    endpoint: "https://ingest.us.signoz.cloud:443"
    protocol: grpc
    headers:
      "signoz-access-token": "${SIGNOZ_TOKEN}"
```

YAML doesn't expand `${...}` itself; substitute at deploy time (helm, envsubst, sops, etc.).

## How the layers compose

The cli installs a single `tracing_subscriber` registry with the layers your config asked for, in order:

1. `RUST_LOG` env filter
2. `fmt` → stderr (when `fmt.enabled`)
3. `sqlite` → `events` + `tool_calls` tables (when `sqlite.enabled`)
4. `otlp` → external collector (when `otlp` is set)

Every span emitted by the running server fans out to all enabled layers. There is no priority or fallback — the SQLite layer keeps full payloads (full prompts, args, results), the OTLP layer ships the same span attributes to your collector. If your backend chokes on multi-megabyte attributes, drop those fields in your collector pipeline rather than at the source.
