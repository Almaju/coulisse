//! HTTP utilities shared by every feature crate's admin router.
//!
//! The admin/studio surface is a representation of the config API:
//! the same routes serve HTML pages, HTML fragments (htmx), or JSON
//! depending on request headers. [`ResponseFormat`] is the extractor
//! that captures that decision so handlers can branch once at the end.
//! [`EitherFormOrJson`] does the symmetric job for request bodies.

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::{StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use serde::de::DeserializeOwned;

/// Which representation the caller wants. Set from the request headers:
/// `HX-Request` → [`Self::Htmx`]; `Accept: application/json` →
/// [`Self::Json`]; otherwise [`Self::Html`]. The cli admin shell
/// middleware wraps `Html` responses in the page chrome and lets `Htmx`
/// fragments through unwrapped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseFormat {
    Html,
    Htmx,
    Json,
}

impl ResponseFormat {
    /// True when the response should be HTML — either as a fragment
    /// (htmx) or as a full page. Useful for handlers that produce the
    /// same HTML body for both and let the shell middleware do the
    /// wrapping.
    pub fn is_html(self) -> bool {
        matches!(self, Self::Html | Self::Htmx)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ResponseFormat {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        if parts.headers.contains_key("hx-request") {
            return Ok(Self::Htmx);
        }
        let prefers_json = parts
            .headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(prefers_json)
            .unwrap_or(false);
        if prefers_json {
            return Ok(Self::Json);
        }
        Ok(Self::Html)
    }
}

/// Cheap Accept-header check. Returns true when JSON appears in the
/// Accept list with strictly higher q-value than HTML, OR when HTML is
/// absent entirely. Browsers send `text/html,...,*/*` so the fallback
/// path keeps them on HTML; `curl -H 'Accept: application/json'` gets
/// JSON.
fn prefers_json(accept: &str) -> bool {
    let mut json_q = -1.0_f32;
    let mut html_q = -1.0_f32;
    for entry in accept.split(',') {
        let mut parts = entry.split(';').map(str::trim);
        let media = match parts.next() {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };
        let mut q = 1.0_f32;
        for param in parts {
            if let Some(v) = param.strip_prefix("q=")
                && let Ok(parsed) = v.parse::<f32>()
            {
                q = parsed;
            }
        }
        match media {
            "application/json" | "*/json" => json_q = json_q.max(q),
            "text/html" | "application/xhtml+xml" | "text/*" | "*/*" => html_q = html_q.max(q),
            _ => {}
        }
    }
    if json_q < 0.0 {
        return false;
    }
    html_q < 0.0 || json_q > html_q
}

/// Body extractor that accepts JSON, YAML, or HTML form encoding and
/// deserializes into the same target type. The admin UI's edit pages
/// send YAML (textarea body, `Content-Type: application/yaml`); the
/// JSON API accepts `application/json`; old-school HTML forms post
/// `application/x-www-form-urlencoded`. Handler bodies stay
/// format-agnostic.
#[derive(Debug)]
pub struct EitherFormOrJson<T>(pub T);

impl<S, T> FromRequest<S> for EitherFormOrJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + 'static,
{
    type Rejection = BodyRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.starts_with("application/yaml")
            || content_type.starts_with("application/x-yaml")
            || content_type.starts_with("text/yaml")
        {
            let bytes = axum::body::to_bytes(req.into_body(), 8 * 1024 * 1024)
                .await
                .map_err(|err| BodyRejection::Yaml(err.to_string()))?;
            let value: T = serde_yaml::from_slice(&bytes)
                .map_err(|err| BodyRejection::Yaml(err.to_string()))?;
            return Ok(Self(value));
        }
        if content_type.starts_with("application/x-www-form-urlencoded") {
            let Form(value) = Form::<T>::from_request(req, state)
                .await
                .map_err(|err| BodyRejection::Form(err.to_string()))?;
            return Ok(Self(value));
        }
        // Default to JSON: `application/json`, missing header, or
        // anything else.
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|err| BodyRejection::Json(err.to_string()))?;
        Ok(Self(value))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BodyRejection {
    #[error("invalid form body: {0}")]
    Form(String),
    #[error("invalid JSON body: {0}")]
    Json(String),
    #[error("invalid YAML body: {0}")]
    Yaml(String),
}

impl IntoResponse for BodyRejection {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, self.to_string()).into_response()
    }
}
