use std::net::{AddrParseError, IpAddr, SocketAddr};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The `server:` block. Every field is optional with a sensible default, so
/// omitting the block entirely yields a server that binds `0.0.0.0:8421`
/// with tokio's default worker-thread count and no request body cap.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[schemars(rename = "ServerConfig")]
pub struct ServerConfig {
    /// IP address to bind. Defaults to `0.0.0.0` (all interfaces). Set to
    /// `127.0.0.1` to refuse connections from anything but loopback — the
    /// right posture for a personal instance fronted by a reverse proxy or
    /// tunnel.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Largest request body the proxy will accept, in bytes. `None` (the
    /// default) leaves axum's built-in 2 MiB limit in place; set a number
    /// to raise or lower it. Guards against a client streaming an unbounded
    /// body into memory.
    #[serde(default)]
    pub max_body_bytes: Option<usize>,
    /// TCP port to bind. Defaults to 8421. Useful when running multiple
    /// Coulisse instances against different `coulisse.yaml` files on the
    /// same machine — give each yaml its own port.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Number of tokio worker threads. `None` (the default) lets tokio size
    /// the pool to the number of CPU cores. Set a fixed number to cap
    /// concurrency on a shared host. Read once at startup, before the
    /// runtime is built — changing it requires a restart.
    #[serde(default)]
    pub worker_threads: Option<usize>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            max_body_bytes: None,
            port: default_port(),
            worker_threads: None,
        }
    }
}

impl ServerConfig {
    /// Resolve the configured bind address and port into a [`SocketAddr`].
    ///
    /// # Errors
    ///
    /// Returns [`BindError`] if `bind` is not a valid IP address.
    pub fn socket_addr(&self) -> Result<SocketAddr, BindError> {
        let ip: IpAddr = self.bind.parse().map_err(|source| BindError {
            source,
            value: self.bind.clone(),
        })?;
        Ok(SocketAddr::new(ip, self.port))
    }

    /// Build the multi-threaded tokio runtime this config describes. Honors
    /// `worker_threads` when set; otherwise tokio sizes the pool to the CPU
    /// count. Called from `main` *before* any async work begins — the worker
    /// count cannot be changed once the runtime exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime fails to build (e.g. the OS refuses
    /// to spawn threads).
    pub fn runtime(&self) -> std::io::Result<tokio::runtime::Runtime> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all();
        if let Some(threads) = self.worker_threads {
            builder.worker_threads(threads.max(1));
        }
        builder.build()
    }

    /// Apply transport-level layers (currently just the request body cap) to
    /// the fully composed application router. A no-op when `max_body_bytes`
    /// is unset, leaving axum's default limit untouched.
    pub fn apply_layers(&self, router: Router) -> Router {
        match self.max_body_bytes {
            None => router,
            Some(bytes) => router.layer(DefaultBodyLimit::max(bytes)),
        }
    }
}

fn default_bind() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8421
}

#[derive(Debug, Error)]
#[error("server.bind is not a valid IP address ({value:?}): {source}")]
pub struct BindError {
    pub source: AddrParseError,
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_bind_all_interfaces_on_8421() {
        let config = ServerConfig::default();
        let addr = config.socket_addr().expect("default bind parses");
        assert_eq!(addr.to_string(), "0.0.0.0:8421");
    }

    #[test]
    fn loopback_bind_parses() {
        let config = ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 9000,
            ..Default::default()
        };
        let addr = config.socket_addr().expect("loopback parses");
        assert_eq!(addr.to_string(), "127.0.0.1:9000");
    }

    #[test]
    fn invalid_bind_is_rejected() {
        let config = ServerConfig {
            bind: "not-an-ip".to_string(),
            ..Default::default()
        };
        assert!(config.socket_addr().is_err());
    }

    #[test]
    fn omitting_block_yields_defaults() {
        let config: ServerConfig = serde_yaml::from_str("{}").expect("empty maps to defaults");
        assert_eq!(config.port, 8421);
        assert_eq!(config.bind, "0.0.0.0");
        assert!(config.worker_threads.is_none());
        assert!(config.max_body_bytes.is_none());
    }
}
