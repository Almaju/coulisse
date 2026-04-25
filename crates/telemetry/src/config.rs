//! YAML slice for the `telemetry:` block. Pure data; the matching
//! subscriber wiring lives in `init.rs`.

use std::collections::HashMap;

use serde::Deserialize;

/// Top-level config. Every field is optional — a missing
/// `telemetry:` block falls back to the documented defaults
/// (fmt to stderr at info, sqlite enabled, no OTLP).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Stderr fmt layer. Defaults: enabled, level inherited from
    /// `RUST_LOG` if set, otherwise `info,sqlx=warn`.
    pub fmt: FmtConfig,
    /// SQLite layer that mirrors spans into the `events` and
    /// `tool_calls` tables for the studio UI. Defaults: enabled.
    /// Disable only if you don't need the studio's per-turn event tree.
    pub sqlite: SqliteConfig,
    /// OpenTelemetry OTLP exporter. Absent (the default) = no
    /// external traces shipped. Set `endpoint` to point at a Grafana,
    /// SigNoz, Jaeger or any OTLP-compatible collector.
    pub otlp: Option<OtlpConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FmtConfig {
    pub enabled: bool,
}

impl Default for FmtConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqliteConfig {
    pub enabled: bool,
}

impl Default for SqliteConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// OTLP exporter knobs. Required when the operator wants to ship
/// traces to their own observability stack.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtlpConfig {
    /// Collector URL — e.g. `http://localhost:4317` for grpc,
    /// `http://localhost:4318/v1/traces` for http/protobuf.
    pub endpoint: String,
    /// Wire protocol. Defaults to `grpc` (port 4317).
    #[serde(default)]
    pub protocol: OtlpProtocol,
    /// Static headers (e.g. `authorization: Bearer ...`) attached to
    /// every export request. Useful for managed backends like SigNoz
    /// Cloud or Honeycomb.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Resource attribute `service.name`. Defaults to `coulisse`.
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

fn default_service_name() -> String {
    "coulisse".to_string()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OtlpProtocol {
    #[default]
    Grpc,
    HttpBinary,
}
