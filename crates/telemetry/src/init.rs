//! Subscriber wiring. Builds a `tracing_subscriber::Registry` with the
//! layers the YAML asked for, returns a guard that holds resources for
//! the process lifetime.
//!
//! Layer composition (outer → inner):
//!   - EnvFilter (`RUST_LOG` overrides, falls back to `info,sqlx=warn`)
//!   - fmt → stderr
//!   - SqliteLayer → `events` / `tool_calls` (drives the studio UI)
//!   - OpenTelemetry → OTLP exporter (optional, opt-in via YAML)

use opentelemetry_sdk::trace::SdkTracerProvider;
use sqlx::SqlitePool;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::config::{Config, OtlpConfig, OtlpProtocol};
use crate::sqlite_layer::{SqliteLayer, SqliteLayerGuard};

/// Held for the process lifetime. Drops the SqliteLayer writer guard
/// (best-effort drain) and shuts down the OTLP exporter so in-flight
/// spans land before the process exits.
pub struct TelemetryGuard {
    /// `Some` when the SQLite layer is enabled; `None` otherwise.
    pub sqlite: Option<SqliteLayerGuard>,
    /// `Some` when OTLP is enabled; flushes the exporter on drop.
    #[allow(dead_code)]
    otlp: Option<OtlpGuard>,
}

struct OtlpGuard {
    provider: SdkTracerProvider,
}

impl Drop for OtlpGuard {
    fn drop(&mut self) {
        // Best-effort flush of pending spans on shutdown. Errors are
        // ignored — the process is already going away and there's
        // nowhere useful to report a failed flush.
        let _ = self.provider.shutdown();
    }
}

/// Initialize the global tracing subscriber from `config`. Calls
/// `tracing_subscriber::registry().init()` internally — must only be
/// invoked once per process.
pub fn init_subscriber(pool: SqlitePool, config: &Config) -> Result<TelemetryGuard, InitError> {
    use opentelemetry::trace::TracerProvider as _;

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));

    let fmt_layer = config
        .fmt
        .enabled
        .then(|| fmt::layer().with_target(false).with_writer(std::io::stderr));

    let (sqlite_layer, sqlite_guard) = if config.sqlite.enabled {
        let (layer, guard) = SqliteLayer::spawn(pool);
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    // OTLP path is built inline so the OpenTelemetryLayer's `S`
    // generic infers from the stacked subscriber type. Splitting the
    // call into two branches avoids the layer's type leaking into a
    // helper signature, which doesn't compose cleanly with the
    // already-stacked `Layered<...>` chain.
    if let Some(cfg) = config.otlp.as_ref() {
        let provider = build_otlp_provider(cfg)?;
        let tracer = provider.tracer("coulisse");
        let otlp_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(sqlite_layer)
            .with(otlp_layer)
            .init();
        Ok(TelemetryGuard {
            sqlite: sqlite_guard,
            otlp: Some(OtlpGuard { provider }),
        })
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(sqlite_layer)
            .init();
        Ok(TelemetryGuard {
            sqlite: sqlite_guard,
            otlp: None,
        })
    }
}

fn build_otlp_provider(cfg: &OtlpConfig) -> Result<SdkTracerProvider, InitError> {
    use opentelemetry_otlp::{WithExportConfig, WithHttpConfig, WithTonicConfig};
    use opentelemetry_sdk::Resource;

    let resource = Resource::builder()
        .with_service_name(cfg.service_name.clone())
        .build();

    let exporter = match cfg.protocol {
        OtlpProtocol::Grpc => {
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&cfg.endpoint);
            if !cfg.headers.is_empty() {
                builder = builder.with_metadata(headers_to_metadata(&cfg.headers)?);
            }
            builder.build().map_err(InitError::Otlp)?
        }
        OtlpProtocol::HttpBinary => {
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(&cfg.endpoint);
            if !cfg.headers.is_empty() {
                builder = builder.with_headers(cfg.headers.clone());
            }
            builder.build().map_err(InitError::Otlp)?
        }
    };

    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build())
}

fn headers_to_metadata(
    headers: &std::collections::HashMap<String, String>,
) -> Result<tonic::metadata::MetadataMap, InitError> {
    let mut metadata = tonic::metadata::MetadataMap::new();
    for (k, v) in headers {
        let key: tonic::metadata::MetadataKey<tonic::metadata::Ascii> =
            k.parse().map_err(|_| InitError::InvalidHeader(k.clone()))?;
        let value = v.parse().map_err(|_| InitError::InvalidHeader(k.clone()))?;
        metadata.insert(key, value);
    }
    Ok(metadata)
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("invalid OTLP header: {0}")]
    InvalidHeader(String),
    #[error("OTLP pipeline init failed: {0}")]
    Otlp(opentelemetry_otlp::ExporterBuildError),
}

impl TelemetryGuard {
    /// Round-trip the SqliteLayer writer (no-op when disabled). Used by
    /// tests that need rows on disk before reading them back.
    pub async fn flush(&self) {
        if let Some(g) = self.sqlite.as_ref() {
            g.flush().await;
        }
        // OTLP batches are flushed by the exporter on drop.
    }
}
