//! Cli-owned pieces of the admin/studio surface.
//!
//! Feature crates render their admin pages as fragments — chrome-free
//! inner HTML, no `<html>` wrapper. This module owns the base layout and
//! the [`shell`] middleware that wraps non-htmx HTML responses in it.
//! Bookmarked deep URLs render with full navigation; htmx-driven
//! navigations stay lean.

use std::sync::Arc;

use askama::Template;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};

use crate::config::Config;

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

#[derive(Template)]
#[template(path = "overview.html")]
struct OverviewPage;

pub async fn overview() -> Result<Html<String>, StatusCode> {
    let html = OverviewPage
        .render()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Html(html))
}

#[derive(Clone)]
pub struct ProviderRow {
    pub kind: String,
    pub masked_key: String,
}

#[derive(Clone)]
pub struct SettingsView {
    pub agent_count: usize,
    pub auth_admin: String,
    pub auth_proxy: String,
    pub experiment_count: usize,
    pub judge_count: usize,
    pub memory_backend: String,
    pub memory_context_budget: u32,
    pub memory_embedder: String,
    pub memory_extractor: String,
    pub providers: Vec<ProviderRow>,
    pub telemetry_fmt: bool,
    pub telemetry_otlp: String,
    pub telemetry_sqlite: bool,
}

impl SettingsView {
    pub fn from_config(config: &Config) -> Self {
        let auth_admin = auth_summary(&config.auth.admin);
        let auth_proxy = auth_summary(&config.auth.proxy);

        let memory_backend = match &config.memory.backend {
            memory::BackendConfig::InMemory => "In-memory (ephemeral)".to_string(),
            memory::BackendConfig::Sqlite { path } => format!("SQLite at {}", path.display()),
        };

        let memory_embedder = match &config.memory.embedder {
            memory::EmbedderConfig::Hash { dims } => format!("hash (dims={dims})"),
            memory::EmbedderConfig::Openai { model, .. } => format!("openai / {model}"),
            memory::EmbedderConfig::Voyage { model, .. } => format!("voyage / {model}"),
        };

        let memory_extractor = config
            .memory
            .extractor
            .as_ref()
            .map(|e| format!("{} / {}", e.provider, e.model))
            .unwrap_or_else(|| "Disabled".to_string());

        let mut providers: Vec<ProviderRow> = config
            .providers
            .iter()
            .map(|(kind, cfg)| {
                let key = &cfg.api_key;
                let masked_key = if key.len() > 4 {
                    format!("····{}", &key[key.len() - 4..])
                } else {
                    "····".to_string()
                };
                ProviderRow {
                    kind: kind.as_str().to_string(),
                    masked_key,
                }
            })
            .collect();
        providers.sort_by(|a, b| a.kind.cmp(&b.kind));

        Self {
            agent_count: config.agents.len(),
            auth_admin,
            auth_proxy,
            experiment_count: config.experiments.len(),
            judge_count: config.judges.len(),
            memory_backend,
            memory_context_budget: config.memory.context_budget.0,
            memory_embedder,
            memory_extractor,
            providers,
            telemetry_fmt: config.telemetry.fmt.enabled,
            telemetry_otlp: config
                .telemetry
                .otlp
                .as_ref()
                .map(|o| o.endpoint.clone())
                .unwrap_or_else(|| "Disabled".to_string()),
            telemetry_sqlite: config.telemetry.sqlite.enabled,
        }
    }
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    settings: SettingsView,
}

pub async fn settings(State(view): State<Arc<SettingsView>>) -> Result<Html<String>, StatusCode> {
    let html = SettingsPage {
        settings: (*view).clone(),
    }
    .render()
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Html(html))
}

fn auth_summary(scope: &Option<auth::ScopeConfig>) -> String {
    match scope {
        None => "Unauthenticated".to_string(),
        Some(s) => {
            if s.basic.is_some() {
                "Basic auth".to_string()
            } else if let Some(oidc) = &s.oidc {
                format!("OIDC ({})", oidc.issuer_url)
            } else {
                "Unconfigured".to_string()
            }
        }
    }
}
