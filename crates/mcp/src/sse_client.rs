//! MCP-over-SSE client transport (older protocol revision).
//!
//! The current MCP spec uses streamable HTTP. Older servers — Atlassian's
//! `mcp.atlassian.com/v1/sse` being the prominent example — still expose
//! the SSE flavour from the previous revision and 404 on `POST /endpoint`
//! attempts. `rmcp` 1.7 ships a streamable-HTTP client and an SSE *server*
//! but no SSE *client*, so Coulisse rolled its own here.
//!
//! Protocol shape (one long-lived GET + N short POSTs):
//!
//! 1. Client opens `GET <url>` with `Accept: text/event-stream` and the
//!    user's `Authorization: Bearer ...` header.
//! 2. Server's first SSE event is `event: endpoint`, `data: <post URL>`.
//!    The `data` is either absolute or relative to the GET URL.
//! 3. Subsequent server events are `event: message`, `data: <JSON-RPC>`.
//! 4. Client `POST`s JSON-RPC requests to the endpoint URL with the same
//!    Authorization header. The server echoes responses back through the
//!    long-lived SSE stream.
//!
//! Implemented as an rmcp `Worker` — one tokio task owns the SSE stream,
//! demuxes server pushes to the rmcp handler, and forwards handler-sent
//! messages out via HTTP POST. The handler-facing `Transport` impl comes
//! for free via `WorkerTransport`.

use std::borrow::Cow;
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures_util::StreamExt as _;
use rmcp::service::{RoleClient, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::worker::{Worker, WorkerConfig, WorkerContext, WorkerQuitReason};
use tokio::sync::oneshot;
use tracing::instrument;

/// HTTP request timeout for the initial endpoint-discovery GET. Atlassian
/// answers within a couple hundred ms; we give a generous ceiling for
/// flaky links but short enough that we don't hang `coulisse start` on a
/// dead server.
const ENDPOINT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub(crate) enum SseClientError {
    #[error("SSE transport closed")]
    Closed,
    #[error("worker join error: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("failed to connect to SSE endpoint {url}: {source}")]
    Connect {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("SSE endpoint {url} returned HTTP {status}")]
    BadStatus { status: u16, url: String },
    #[error(
        "SSE stream from {url} ended before sending the initial `endpoint` event \
         (the server doesn't speak the MCP-over-SSE protocol)"
    )]
    MissingEndpointEvent { url: String },
    #[error("SSE endpoint URL {raw} is not absolute and not joinable against {base}")]
    BadEndpointUrl { base: String, raw: String },
    #[error("SSE event stream error: {0}")]
    Stream(String),
    #[error("failed to POST message to {url}: {source}")]
    Post {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("POST to {url} returned HTTP {status}: {body}")]
    PostStatus {
        body: String,
        status: u16,
        url: String,
    },
    #[error("failed to serialize outgoing message: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Builder for an MCP-over-SSE client. Call `.connect()` to perform the
/// initial GET handshake, then `.into_transport()` to wrap as an rmcp
/// `Transport` suitable for `().serve(transport).await`.
pub(crate) struct SseClientTransport {
    auth_header: Option<String>,
    base_url: String,
    client: reqwest::Client,
    post_url: String,
    sse_stream: Box<
        dyn futures_util::Stream<Item = Result<eventsource_stream::Event, String>> + Send + Unpin,
    >,
}

impl SseClientTransport {
    /// Open the long-lived SSE GET and read the initial `endpoint` event.
    /// On success, the transport is ready to send and receive messages.
    ///
    /// # Errors
    ///
    /// Returns `SseClientError::Connect` / `BadStatus` if the GET fails,
    /// or `MissingEndpointEvent` if the server doesn't speak MCP-over-SSE.
    #[instrument(skip(auth_header))]
    pub(crate) async fn connect(
        url: &str,
        auth_header: Option<String>,
    ) -> Result<Self, SseClientError> {
        let client = reqwest::Client::builder()
            // No global timeout — the GET stays open for the lifetime of
            // the session. We bound only the initial connect attempt
            // below.
            .build()
            .map_err(|source| SseClientError::Connect {
                url: url.to_string(),
                source,
            })?;
        let mut req = client
            .get(url)
            .header("Accept", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .timeout(ENDPOINT_DISCOVERY_TIMEOUT);
        if let Some(h) = auth_header.as_deref() {
            req = req.header("Authorization", h);
        }
        let response = req.send().await.map_err(|source| SseClientError::Connect {
            url: url.to_string(),
            source,
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(SseClientError::BadStatus {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }
        // From here on, no timeout — the stream is supposed to stay open.
        let bytes = response.bytes_stream();
        let mut events = Box::new(bytes.eventsource().map(|r| r.map_err(|e| e.to_string())));
        // First event MUST be `event: endpoint` with `data: <post URL>`.
        // mcp-remote does the same probe and falls back to streamable-HTTP
        // if missing. Coulisse caller does the inverse: caller tries
        // streamable-HTTP first, then us; so if we don't see endpoint,
        // it's a real protocol error.
        let post_url = loop {
            let next = events.next().await;
            match next {
                Some(Ok(ev)) if ev.event == "endpoint" => {
                    break resolve_endpoint(url, ev.data.trim())?;
                }
                Some(Ok(_)) => {
                    // Some servers send pings or comments before the
                    // endpoint event. Keep looping for the real one.
                }
                Some(Err(e)) => return Err(SseClientError::Stream(e)),
                None => {
                    return Err(SseClientError::MissingEndpointEvent {
                        url: url.to_string(),
                    });
                }
            }
        };
        Ok(Self {
            auth_header,
            base_url: url.to_string(),
            client,
            post_url,
            sse_stream: events,
        })
    }
}

/// Resolve the `data` payload of the `endpoint` event to an absolute URL.
/// Servers send either an absolute URL or a relative path against the
/// original GET URL.
fn resolve_endpoint(base: &str, raw: &str) -> Result<String, SseClientError> {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Ok(raw.to_string());
    }
    let base_url = reqwest::Url::parse(base).map_err(|_| SseClientError::BadEndpointUrl {
        base: base.to_string(),
        raw: raw.to_string(),
    })?;
    base_url
        .join(raw)
        .map(|u| u.to_string())
        .map_err(|_| SseClientError::BadEndpointUrl {
            base: base.to_string(),
            raw: raw.to_string(),
        })
}

impl Worker for SseClientTransport {
    type Error = SseClientError;
    type Role = RoleClient;

    fn err_closed() -> Self::Error {
        SseClientError::Closed
    }

    fn err_join(e: tokio::task::JoinError) -> Self::Error {
        SseClientError::Join(e)
    }

    fn config(&self) -> WorkerConfig {
        let mut cfg = WorkerConfig::default();
        cfg.name = Some(format!("sse-client:{}", self.base_url));
        cfg
    }

    async fn run(
        mut self,
        mut context: WorkerContext<Self>,
    ) -> Result<(), WorkerQuitReason<Self::Error>> {
        loop {
            tokio::select! {
                () = context.cancellation_token.cancelled() => {
                    return Err(WorkerQuitReason::Cancelled);
                }
                next = self.sse_stream.next() => {
                    match next {
                        Some(Ok(ev)) => {
                            // Only `event: message` carries JSON-RPC.
                            // The spec also allows unnamed/`message` events
                            // (default), so accept both.
                            if !ev.event.is_empty() && ev.event != "message" {
                                continue;
                            }
                            let trimmed = ev.data.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<RxJsonRpcMessage<RoleClient>>(trimmed) {
                                Ok(msg) => {
                                    context.send_to_handler(msg).await?;
                                }
                                Err(e) => {
                                    return Err(WorkerQuitReason::fatal(
                                        SseClientError::Serialize(e),
                                        Cow::Borrowed("parsing SSE event data as JSON-RPC"),
                                    ));
                                }
                            }
                        }
                        Some(Err(e)) => {
                            return Err(WorkerQuitReason::fatal(
                                SseClientError::Stream(e),
                                Cow::Borrowed("SSE stream"),
                            ));
                        }
                        None => {
                            return Err(WorkerQuitReason::TransportClosed);
                        }
                    }
                }
                req = context.from_handler_rx.recv() => {
                    let Some(req) = req else {
                        return Err(WorkerQuitReason::HandlerTerminated);
                    };
                    let result = post_message(
                        &self.client,
                        &self.post_url,
                        self.auth_header.as_deref(),
                        &req.message,
                    )
                    .await;
                    // Worker swallows responder errors — the handler may
                    // have dropped the oneshot. That's not fatal.
                    let _ = req.responder.send(result);
                }
            }
        }
    }
}

async fn post_message(
    client: &reqwest::Client,
    url: &str,
    auth_header: Option<&str>,
    msg: &TxJsonRpcMessage<RoleClient>,
) -> Result<(), SseClientError> {
    let body = serde_json::to_vec(msg)?;
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body);
    if let Some(h) = auth_header {
        req = req.header("Authorization", h);
    }
    let response = req.send().await.map_err(|source| SseClientError::Post {
        url: url.to_string(),
        source,
    })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(SseClientError::PostStatus {
            body,
            status: status.as_u16(),
            url: url.to_string(),
        });
    }
    Ok(())
}

// `oneshot` isn't used in this module but is referenced via the worker
// traits transitively; keeping the import explicit so a future direct use
// (e.g. graceful shutdown signal) doesn't have to rediscover it.
#[allow(dead_code)]
fn _ensure_oneshot_in_scope() {
    let (_tx, _rx) = oneshot::channel::<()>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_endpoint_url_round_trips() {
        let resolved = resolve_endpoint(
            "https://mcp.example.com/v1/sse",
            "https://mcp.example.com/v1/messages/abc",
        )
        .unwrap();
        assert_eq!(resolved, "https://mcp.example.com/v1/messages/abc");
    }

    /// Some servers (Atlassian) return a relative path against the SSE
    /// URL. RFC 3986 URL joining must produce an absolute URL we can POST
    /// to directly.
    #[test]
    fn relative_endpoint_resolves_against_base() {
        let resolved = resolve_endpoint(
            "https://mcp.example.com/v1/sse",
            "/v1/messages/abc?session=xyz",
        )
        .unwrap();
        assert_eq!(
            resolved,
            "https://mcp.example.com/v1/messages/abc?session=xyz"
        );
    }

    /// Path-relative form (no leading slash) joins against the SSE URL's
    /// directory.
    #[test]
    fn path_relative_endpoint_resolves() {
        let resolved = resolve_endpoint("https://mcp.example.com/v1/sse", "messages/abc").unwrap();
        assert_eq!(resolved, "https://mcp.example.com/v1/messages/abc");
    }

    #[test]
    fn unparseable_base_url_errors() {
        let err = resolve_endpoint("not a url", "relative").unwrap_err();
        assert!(matches!(err, SseClientError::BadEndpointUrl { .. }));
    }
}
