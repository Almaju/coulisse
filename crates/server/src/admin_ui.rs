//! Static serving for the admin Leptos app. The WASM bundle is produced by
//! `trunk build` from `crates/admin/` and embedded into the server binary at
//! compile time via `rust-embed`.
//!
//! If the embedded folder is empty (no `trunk build` was run), we serve a
//! friendly placeholder instead of an opaque 404 — surfacing the build step
//! to the operator rather than failing silently.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::Response;
use axum::routing::get;
use prompter::Prompter;
use rust_embed::RustEmbed;

use crate::AppState;

#[derive(RustEmbed)]
#[folder = "../admin/dist/"]
struct AdminAssets;

pub fn router<P: Prompter + 'static>() -> Router<Arc<AppState<P>>> {
    Router::new()
        .route("/", get(index::<P>))
        .route("/{*path}", get(asset::<P>))
}

async fn index<P: Prompter>(State(_): State<Arc<AppState<P>>>) -> Response {
    serve("index.html")
}

async fn asset<P: Prompter>(
    State(_): State<Arc<AppState<P>>>,
    Path(path): Path<String>,
    uri: Uri,
) -> Response {
    let _ = uri;
    // Client-side router owns everything under `/admin`. Non-asset paths
    // (`/admin/users/:id`) must fall back to `index.html` so the SPA picks
    // them up on load. Real assets carry an extension; route paths don't.
    if path.contains('.') {
        serve(&path)
    } else {
        serve("index.html")
    }
}

fn serve(path: &str) -> Response {
    let Some(file) = AdminAssets::get(path) else {
        return placeholder();
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut response = Response::new(Body::from(file.data.into_owned()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response
}

/// Shown when the admin bundle wasn't built (`dist/` is empty). Keeps the
/// binary usable — `/admin/api/*` still works — and tells the operator
/// exactly what command to run.
fn placeholder() -> Response {
    let body = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Coulisse admin</title>
<style>
body { font-family: ui-sans-serif, system-ui, sans-serif; color: #0f172a; background: #f8fafc; padding: 48px; max-width: 640px; margin: 0 auto; }
h1 { font-size: 20px; margin: 0 0 12px; }
code { background: #e2e8f0; padding: 2px 6px; border-radius: 4px; font-size: 13px; }
.muted { color: #64748b; }
</style></head>
<body>
<h1>Admin UI not built</h1>
<p class="muted">The Coulisse admin UI is a Leptos WASM app that must be built separately. Run:</p>
<pre><code>cd crates/admin && trunk build --release</code></pre>
<p class="muted">Then restart the server so the new bundle is embedded. The JSON API at <code>/admin/api/*</code> is already live.</p>
</body></html>"#;
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    *response.status_mut() = StatusCode::OK;
    response
}
