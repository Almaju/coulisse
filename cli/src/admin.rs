//! Cli-owned pieces of the admin/studio surface.
//!
//! Feature crates render their admin pages as fragments — chrome-free
//! inner HTML, no `<html>` wrapper. This module owns the base layout and
//! the [`shell`] middleware that wraps non-htmx HTML responses in it.
//! Bookmarked deep URLs render with full navigation; htmx-driven
//! navigations stay lean.

use std::sync::Arc;

use arc_swap::ArcSwap;
use askama::Template;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use coulisse_core::{ConfigPersistError, ConfigPersister, EitherFormOrJson, ResponseFormat};
use serde_yaml::Value;

use crate::config::Config;
use crate::config_store::ConfigStore;

/// Hot-reloadable handle for the cli-owned settings summary. The
/// underlying view is rebuilt from `Config` whenever the YAML changes
/// (admin save or hand-edit), so the `/admin/settings` page always
/// reflects what's actually live on disk.
pub type SettingsHandle = Arc<ArcSwap<SettingsView>>;

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
    let from_htmx = request.headers().contains_key("hx-request");
    let response = next.run(request).await;
    if from_htmx || !response.status().is_success() {
        return response;
    }
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if !is_html {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to buffer admin response: {err}"),
            )
                .into_response();
        }
        Ok(b) => b,
    };
    let inner = String::from_utf8_lossy(&bytes);
    let html = match (BaseShell { content: &inner }).render() {
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("base layout render failed: {err}"),
            )
                .into_response();
        }
        Ok(s) => s,
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(html))
}

pub mod live;

/// Embedded studio assets, served under `/admin/static/`. Everything the
/// shell needs ships inside the binary — no CDN, no network dependency —
/// so the studio renders identically offline and behind strict CSPs.
/// The shell middleware passes non-HTML responses through untouched, so
/// each asset's content type survives.
const APP_JS: &str = include_str!("../static/app.js");
const COULISSE_CSS: &str = include_str!("../static/coulisse.css");
/// Vendored fonts (SIL Open Font License): Bricolage Grotesque and
/// Hanken Grotesk as latin variable fonts, IBM Plex Mono as latin
/// static weights.
const FONT_BRICOLAGE: &[u8] = include_bytes!("../static/fonts/bricolage-grotesque-latin.woff2");
const FONT_HANKEN: &[u8] = include_bytes!("../static/fonts/hanken-grotesk-latin.woff2");
const FONT_PLEX_MONO_400: &[u8] = include_bytes!("../static/fonts/ibm-plex-mono-latin-400.woff2");
const FONT_PLEX_MONO_500: &[u8] = include_bytes!("../static/fonts/ibm-plex-mono-latin-500.woff2");
const HTMX_JS: &str = include_str!("../static/htmx.min.js");

/// Static assets for the studio shell. Kept on the cli side because cli
/// owns `base.html` and the chrome these assets enhance.
pub fn static_router() -> Router {
    Router::new()
        .route("/static/app.js", get(js_asset(APP_JS)))
        .route("/static/coulisse.css", get(css_asset(COULISSE_CSS)))
        .route(
            "/static/fonts/bricolage-grotesque-latin.woff2",
            get(font_asset(FONT_BRICOLAGE)),
        )
        .route(
            "/static/fonts/hanken-grotesk-latin.woff2",
            get(font_asset(FONT_HANKEN)),
        )
        .route(
            "/static/fonts/ibm-plex-mono-latin-400.woff2",
            get(font_asset(FONT_PLEX_MONO_400)),
        )
        .route(
            "/static/fonts/ibm-plex-mono-latin-500.woff2",
            get(font_asset(FONT_PLEX_MONO_500)),
        )
        .route("/static/htmx.min.js", get(js_asset(HTMX_JS)))
}

/// Immutable + max-age lets the browser skip re-fetching embedded assets
/// across htmx navigations; the URL set is versioned with the binary.
const STATIC_CACHE: &str = "public, max-age=86400";

fn css_asset(body: &'static str) -> impl Fn() -> ReadyResponse + Clone {
    static_asset(body.as_bytes(), "text/css; charset=utf-8")
}

fn font_asset(body: &'static [u8]) -> impl Fn() -> ReadyResponse + Clone {
    static_asset(body, "font/woff2")
}

fn js_asset(body: &'static str) -> impl Fn() -> ReadyResponse + Clone {
    static_asset(body.as_bytes(), "application/javascript; charset=utf-8")
}

type ReadyResponse = std::future::Ready<Response>;

fn static_asset(
    body: &'static [u8],
    content_type: &'static str,
) -> impl Fn() -> ReadyResponse + Clone {
    move || {
        std::future::ready(
            (
                [
                    (header::CACHE_CONTROL, STATIC_CACHE),
                    (header::CONTENT_TYPE, content_type),
                ],
                body,
            )
                .into_response(),
        )
    }
}

/// State for the Home page: the hot-reloadable settings summary for
/// config counts, plus the telemetry sink for 24h activity numbers.
/// Cross-feature composition lives in cli, per the project rule.
#[derive(Clone)]
pub struct HomeState {
    pub settings: SettingsHandle,
    pub telemetry: Arc<telemetry::Sink>,
}

impl HomeState {
    pub fn router(self) -> Router {
        Router::new().route("/overview", get(home)).with_state(self)
    }
}

#[derive(Template)]
#[template(path = "overview.html")]
struct HomePage {
    agent_count: usize,
    experiment_count: usize,
    judge_count: usize,
    turns_24h: u32,
    users_24h: u32,
}

const DAY_SECS: u64 = 86_400;

async fn home(State(state): State<HomeState>) -> Result<Html<String>, PageError> {
    let since = coulisse_core::now_secs().saturating_sub(DAY_SECS);
    let activity = state.telemetry.recent_activity_counts(since).await?;
    let settings = state.settings.load_full();
    let html = HomePage {
        agent_count: settings.agent_count,
        experiment_count: settings.experiment_count,
        judge_count: settings.judge_count,
        turns_24h: activity.turn_count,
        users_24h: activity.user_count,
    }
    .render()?;
    Ok(Html(html))
}

/// Why a cli-owned studio page could not be produced. Every variant is a
/// server-side failure: the cause is logged and the client sees a 500.
#[derive(Debug, thiserror::Error)]
pub enum PageError {
    #[error("page render failed: {0}")]
    Render(#[from] askama::Error),
    #[error("telemetry read failed: {0}")]
    Telemetry(#[from] telemetry::TelemetryError),
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self, "studio page request failed");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

#[derive(Clone)]
pub struct ProviderRow {
    pub kind: String,
    pub masked_key: String,
}

/// How long-term user memory is configured, as the settings page shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserStateSummary {
    Custom,
    Disabled,
    Enabled,
}

impl UserStateSummary {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Custom => "Enabled (custom)",
            Self::Disabled => "Disabled",
            Self::Enabled => "Enabled (auto)",
        }
    }
}

impl From<&memory::UserStateYaml> for UserStateSummary {
    fn from(yaml: &memory::UserStateYaml) -> Self {
        match yaml {
            memory::UserStateYaml::Configured(_) => Self::Custom,
            memory::UserStateYaml::OnOff(false) => Self::Disabled,
            memory::UserStateYaml::OnOff(true) => Self::Enabled,
        }
    }
}

impl std::fmt::Display for UserStateSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone)]
pub struct SettingsView {
    pub agent_count: usize,
    pub auth_admin: String,
    pub auth_proxy: String,
    pub experiment_count: usize,
    pub judge_count: usize,
    pub memory_extractor: String,
    pub memory_storage: String,
    pub memory_user_state: UserStateSummary,
    pub providers: Vec<ProviderRow>,
    pub telemetry_fmt: bool,
    pub telemetry_otlp: String,
    pub telemetry_sqlite: bool,
}

impl SettingsView {
    #[must_use]
    pub fn from_config(config: &Config, memory_config: &memory::MemoryConfig) -> Self {
        let auth_admin = Self::auth_summary(config.auth.admin.as_ref());
        let auth_proxy = Self::auth_summary(config.auth.proxy.as_ref());

        let memory_storage = match &memory_config.backend {
            memory::BackendConfig::InMemory => "In-memory (ephemeral)".to_string(),
            memory::BackendConfig::Sqlite { path } => path.display().to_string(),
        };

        let memory_extractor = memory_config.extractor.as_ref().map_or_else(
            || "Disabled".to_string(),
            |e| format!("{} / {}", e.provider, e.model),
        );

        let mut providers: Vec<ProviderRow> = config
            .providers
            .iter()
            .map(|(kind, cfg)| ProviderRow {
                kind: kind.as_str().to_string(),
                masked_key: mask_key(&cfg.api_key),
            })
            .collect();
        providers.sort_by(|a, b| a.kind.cmp(&b.kind));

        Self {
            agent_count: config.agents.len(),
            auth_admin,
            auth_proxy,
            experiment_count: config.experiments.len(),
            judge_count: config.judges.len(),
            memory_extractor,
            memory_storage,
            memory_user_state: UserStateSummary::from(&config.memory.user_state),
            providers,
            telemetry_fmt: config.telemetry.fmt.enabled,
            telemetry_otlp: config
                .telemetry
                .otlp
                .as_ref()
                .map_or_else(|| "Disabled".to_string(), |o| o.endpoint.clone()),
            telemetry_sqlite: config.telemetry.sqlite.enabled,
        }
    }

    fn auth_summary(scope: Option<&auth::ScopeConfig>) -> String {
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
}

/// Show only the last four characters of an API key, or nothing at all
/// for keys too short to keep a tail private.
pub(crate) fn mask_key(key: &str) -> String {
    if key.len() > 4 {
        format!("····{}", &key[key.len() - 4..])
    } else {
        "····".to_string()
    }
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    settings: SettingsView,
}

/// # Errors
///
/// Returns an error if the underlying operation fails.
pub async fn settings(State(view): State<SettingsHandle>) -> Result<Html<String>, PageError> {
    let snapshot = view.load_full();
    let html = SettingsPage {
        settings: (*snapshot).clone(),
    }
    .render()?;
    Ok(Html(html))
}

#[derive(Template)]
#[template(path = "config_edit.html")]
struct ConfigEditPage {
    yaml: String,
}

impl ConfigStore {
    /// Whole-file config endpoint. `GET` returns the YAML (or JSON when
    /// the client asks for JSON via Accept). `PUT` replaces the file
    /// atomically with the supplied body — accepts JSON, YAML, or form
    /// encoding via the same body extractor. Power-user equivalent of
    /// `git pull && systemctl reload coulisse`, but via HTTP and with the
    /// validator running before anything touches disk.
    pub fn file_router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/config", get(Self::get_config).put(Self::put_config))
            .route("/config/edit", get(Self::edit_config))
            .with_state(self)
    }

    /// Full-file YAML editor page. Posts to `PUT /config` (`write_all`),
    /// which validates the whole config before replacing the file.
    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn edit_config(
        State(store): State<Arc<Self>>,
    ) -> Result<Html<String>, ConfigEndpointError> {
        let yaml =
            std::fs::read_to_string(store.path()).map_err(|source| ConfigEndpointError::Read {
                path: store.path().display().to_string(),
                source,
            })?;
        let html = ConfigEditPage { yaml }.render()?;
        Ok(Html(html))
    }

    async fn get_config(
        State(store): State<Arc<Self>>,
        fmt: ResponseFormat,
    ) -> Result<Response, ConfigEndpointError> {
        let bytes = std::fs::read(store.path()).map_err(|source| ConfigEndpointError::Read {
            path: store.path().display().to_string(),
            source,
        })?;
        if matches!(fmt, ResponseFormat::Json) {
            let value: Value = serde_yaml::from_slice(&bytes)?;
            let json: serde_json::Value = serde_json::to_value(&value)?;
            return Ok(Json(json).into_response());
        }
        let text = String::from_utf8(bytes)?;
        let mut resp = text.into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/yaml; charset=utf-8"),
        );
        Ok(resp)
    }

    async fn put_config(
        State(store): State<Arc<Self>>,
        EitherFormOrJson(value): EitherFormOrJson<Value>,
    ) -> Result<Response, ConfigEndpointError> {
        store.write_all(value).await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigEndpointError {
    #[error("config file is not valid UTF-8: {0}")]
    Encoding(#[from] std::string::FromUtf8Error),
    #[error("config file is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Persist(#[from] ConfigPersistError),
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("config editor render failed: {0}")]
    Render(#[from] askama::Error),
    #[error("config file is not valid YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl IntoResponse for ConfigEndpointError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Encoding(_)
            | Self::Json(_)
            | Self::Yaml(_)
            | Self::Persist(ConfigPersistError::Parse(_)) => StatusCode::BAD_REQUEST,
            Self::Persist(ConfigPersistError::Invalid(_)) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Persist(ConfigPersistError::Io(_)) | Self::Read { .. } | Self::Render(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "config endpoint failed");
        }
        (status, self.to_string()).into_response()
    }
}
