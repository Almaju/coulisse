//! Subscriber wiring. Builds a `tracing_subscriber::Registry` with the
//! layers the YAML asked for, returns a guard that holds resources for
//! the process lifetime.
//!
//! Layer composition (outer → inner):
//!   - `EnvFilter` (`RUST_LOG` overrides, falls back to `info,sqlx=warn`)
//!   - fmt → stderr
//!   - `SqliteLayer` → `events` / `tool_calls` (drives the studio UI)
//!   - OpenTelemetry → OTLP exporter (optional, opt-in via YAML)

use opentelemetry_sdk::trace::SdkTracerProvider;
use sqlx::SqlitePool;
use tonic::metadata::errors::{InvalidMetadataKey, InvalidMetadataValue};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::config::{Config, OtlpConfig, OtlpProtocol};
use crate::sqlite_layer::{SqliteLayer, SqliteLayerGuard};

const DEFAULT_DIRECTIVES: &str = "info,sqlx=warn";

/// Held for the process lifetime. Drops the `SqliteLayer` writer guard
/// (best-effort drain) and shuts down the OTLP exporter so in-flight
/// spans land before the process exits.
pub struct TelemetryGuard {
    /// `Some` when OTLP is enabled; flushes the exporter on drop.
    #[allow(dead_code)]
    otlp: Option<OtlpGuard>,
    /// `Some` when the `SQLite` layer is enabled; `None` otherwise.
    pub sqlite: Option<SqliteLayerGuard>,
}

struct OtlpGuard {
    provider: SdkTracerProvider,
}

impl Drop for OtlpGuard {
    fn drop(&mut self) {
        // WHY: errors are ignored on shutdown — the process is already
        // going away and there's nowhere useful to report them.
        let _ = self.provider.shutdown();
    }
}

impl Config {
    /// Initialize the global tracing subscriber from this config. Calls
    /// `tracing_subscriber::registry().init()` internally — must only be
    /// invoked once per process.
    ///
    /// # Errors
    ///
    /// Returns an error if the OTLP exporter cannot be built.
    pub fn init_subscriber(&self, pool: SqlitePool) -> Result<TelemetryGuard, InitError> {
        use opentelemetry::trace::TracerProvider as _;

        let env_filter = env_filter_from_env();

        let fmt_layer = self
            .fmt
            .enabled
            .then(|| fmt::layer().with_target(false).with_writer(std::io::stderr));

        let (sqlite_layer, sqlite_guard) = if self.sqlite.enabled {
            let (layer, guard) = SqliteLayer::spawn(pool);
            (Some(layer), Some(guard))
        } else {
            (None, None)
        };

        // WHY: OTLP path is built inline so OpenTelemetryLayer's `S` generic
        // infers from the stacked subscriber type. Extracting to a helper
        // leaks the layer's type and doesn't compose with the existing
        // `Layered<...>` chain.
        if let Some(cfg) = self.otlp.as_ref() {
            let provider = cfg.build_provider()?;
            let tracer = provider.tracer("coulisse");
            let otlp_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt_layer)
                .with(sqlite_layer)
                .with(otlp_layer)
                .init();
            Ok(TelemetryGuard {
                otlp: Some(OtlpGuard { provider }),
                sqlite: sqlite_guard,
            })
        } else {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt_layer)
                .with(sqlite_layer)
                .init();
            Ok(TelemetryGuard {
                otlp: None,
                sqlite: sqlite_guard,
            })
        }
    }
}

/// `RUST_LOG` when set and well-formed, the built-in default otherwise.
/// A malformed value is reported on stderr: no subscriber exists yet, so
/// `tracing` cannot carry the message.
fn env_filter_from_env() -> EnvFilter {
    let Some(spec) = std::env::var_os(EnvFilter::DEFAULT_ENV) else {
        return EnvFilter::new(DEFAULT_DIRECTIVES);
    };
    let spec = spec.to_string_lossy();
    EnvFilter::try_new(spec.as_ref()).unwrap_or_else(|err| {
        eprintln!(
            "telemetry: ignoring {}={spec:?} ({err}); using {DEFAULT_DIRECTIVES:?}",
            EnvFilter::DEFAULT_ENV
        );
        EnvFilter::new(DEFAULT_DIRECTIVES)
    })
}

impl OtlpConfig {
    fn build_provider(&self) -> Result<SdkTracerProvider, InitError> {
        use opentelemetry_otlp::{WithExportConfig, WithHttpConfig, WithTonicConfig};
        use opentelemetry_sdk::Resource;

        let resource = Resource::builder()
            .with_service_name(self.service_name.clone())
            .build();

        let exporter = match self.protocol {
            OtlpProtocol::Grpc => {
                let mut builder = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(&self.endpoint);
                if !self.headers.is_empty() {
                    builder = builder.with_metadata(self.grpc_metadata()?);
                }
                builder.build()?
            }
            OtlpProtocol::HttpBinary => {
                let mut builder = opentelemetry_otlp::SpanExporter::builder()
                    .with_http()
                    .with_endpoint(&self.endpoint);
                if !self.headers.is_empty() {
                    builder = builder.with_headers(self.headers.clone());
                }
                builder.build()?
            }
        };

        Ok(SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build())
    }

    fn grpc_metadata(&self) -> Result<tonic::metadata::MetadataMap, InitError> {
        let mut metadata = tonic::metadata::MetadataMap::new();
        for (k, v) in &self.headers {
            let key: tonic::metadata::MetadataKey<tonic::metadata::Ascii> =
                k.parse().map_err(|source| InitError::InvalidHeaderName {
                    name: k.clone(),
                    source,
                })?;
            let value = v.parse().map_err(|source| InitError::InvalidHeaderValue {
                name: k.clone(),
                source,
            })?;
            metadata.insert(key, value);
        }
        Ok(metadata)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("invalid OTLP header name {name:?}: {source}")]
    InvalidHeaderName {
        name: String,
        #[source]
        source: InvalidMetadataKey,
    },
    #[error("invalid OTLP header value for {name:?}: {source}")]
    InvalidHeaderValue {
        name: String,
        #[source]
        source: InvalidMetadataValue,
    },
    #[error("OTLP pipeline init failed: {0}")]
    Otlp(#[from] opentelemetry_otlp::ExporterBuildError),
}

impl TelemetryGuard {
    /// Round-trip the `SqliteLayer` writer (no-op when disabled). Used by
    /// tests that need rows on disk before reading them back.
    pub async fn flush(&self) {
        if let Some(g) = self.sqlite.as_ref() {
            g.flush().await;
        }
        // NOTE: OTLP batches are flushed by the exporter on drop.
    }
}
