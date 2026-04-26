//! Cli-owned pieces of the admin/studio surface.
//!
//! Feature crates render their admin pages as fragments — chrome-free
//! inner HTML, no `<html>` wrapper. This module owns the base layout and
//! the [`shell`] middleware that wraps non-htmx HTML responses in it.
//! Bookmarked deep URLs render with full navigation; htmx-driven
//! navigations stay lean.

use askama::Template;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

#[derive(Template)]
#[template(path = "base.html")]
struct BaseShell<'a> {
    content: &'a str,
}

/// Tower middleware: wrap non-htmx 2xx HTML responses in the base layout.
/// Pass-through for htmx requests (`HX-Request` header), non-2xx
/// responses, and non-HTML content. Streamed responses are buffered;
/// admin pages are small enough that buffering is fine.
pub async fn shell(request: Request, next: Next) -> Response {
    let is_htmx = request.headers().contains_key("hx-request");
    let response = next.run(request).await;
    if is_htmx || !response.status().is_success() {
        return response;
    }
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("text/html"))
        .unwrap_or(false);
    if !is_html {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to buffer admin response: {err}"),
            )
                .into_response();
        }
    };
    let inner = String::from_utf8_lossy(&bytes);
    let html = match (BaseShell { content: &inner }).render() {
        Ok(s) => s,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("base layout render failed: {err}"),
            )
                .into_response();
        }
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(html))
}
