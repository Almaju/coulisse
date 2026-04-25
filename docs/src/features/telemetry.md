# Telemetry

Coulisse emits its own observability via the `tracing` crate. Every request opens a `turn` span; every tool invocation (MCP or subagent) opens a child `tool_call` span. The configured layers — fmt, SQLite, and optionally OTLP — receive those spans and route them where you've asked for.

The result: the studio UI gives you an offline audit trail, and any OpenTelemetry-compatible backend (Grafana, SigNoz, Jaeger, Honeycomb, ...) gives you live traces. They're driven from the same source — there's no separate path.

## Span model

| Span name   | Opened when                                | Fields                                                                |
|-------------|--------------------------------------------|-----------------------------------------------------------------------|
| `turn`      | a chat completion request arrives          | `agent`, `experiment` (when applicable), `turn_id`, `user_id`, `user_message` |
| `tool_call` | an MCP or subagent tool fires              | `args`, `error` (on failure), `kind` (`mcp`/`subagent`), `result`, `tool_name` |
| `llm_call`  | (reserved) a single LLM provider round-trip | `provider`, `model`, `prompt`, `response`, `usage`                    |

`turn` is the root; `tool_call` and `llm_call` nest under it via the tracing span tree, so OTLP backends render them as a trace tree out of the box.

## Studio integration

When `telemetry.sqlite.enabled` is `true` (the default), the studio's per-turn event tree and tool-call panel render directly from the same spans. Nothing extra to wire up — open `/studio` and the tree is there.

## OTLP backends

Set `telemetry.otlp.endpoint` to start exporting. The exporter batches spans, retries on transient failures, and shuts down cleanly on process exit so in-flight spans land before the server stops.

Tested with:

- **Grafana** (Tempo / Cloud) — gRPC at `4317`.
- **SigNoz** (self-hosted or Cloud) — gRPC; for Cloud add a `signoz-access-token` header.
- **Jaeger** — gRPC at `4317` (Jaeger ≥ 1.50 speaks OTLP natively).
- **Honeycomb** — HTTP/protobuf at `https://api.honeycomb.io/v1/traces` with `x-honeycomb-team` header.

## Tuning verbosity

The fmt layer (stderr logs) is controlled by `RUST_LOG`:

```bash
RUST_LOG=info,sqlx=warn coulisse        # default
RUST_LOG=debug coulisse                 # verbose, including SQL driver
RUST_LOG=warn coulisse                  # quiet
RUST_LOG=coulisse=debug,agents=trace coulisse   # per-crate filtering
```

The SQLite and OTLP layers are not affected by `RUST_LOG` — they capture every `turn` / `tool_call` / `llm_call` span regardless of log level.

## Disabling layers

Each layer has its own `enabled` flag. Common combinations:

```yaml
# Production with external observability stack
telemetry:
  sqlite:
    enabled: false      # studio not exposed; no need to keep DB rows
  otlp:
    endpoint: "..."
```

```yaml
# Local development, no external backend
telemetry:
  # default fmt + sqlite
```

```yaml
# CI / load tests — minimize logging overhead
telemetry:
  fmt:
    enabled: false
  sqlite:
    enabled: false
```
