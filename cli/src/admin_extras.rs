//! Cli-owned admin endpoints for the config sections edited straight
//! through the `ConfigStore` rather than a feature crate's database:
//! the `providers` and `mcp` collections, and the `auth`, `memory`,
//! `storage` and `telemetry` singletons. These edits only need the
//! shared `ConfigPersister` and the section's own serde shape — no
//! feature-crate database — so they live at the config layer that owns
//! `coulisse.yaml` rather than with the feature crates' runtime/data
//! admin pages.
//!
//! Same shape as the per-feature admin routers — content negotiation,
//! JSON/YAML/form body parsing, write-through to `coulisse.yaml`. Edits
//! land in the file and refresh the admin display via the `ConfigStore`
//! snapshot; the runtime that consumes them is built at boot and still
//! requires a restart to swap.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use coulisse_core::{
    ConfigPersistError, ConfigPersister, EitherFormOrJson, ResponseFormat, redirect_to,
};
use mcp::{McpServerConfig, McpTransport};
use providers::{ProviderConfig, ProviderKind};
use serde::Deserialize;

use crate::admin::mask_key;
use crate::config_store::ConfigStore;

impl ConfigStore {
    /// Routes for the config sections that are edited in the YAML file
    /// directly: `providers`, `mcp`, `auth`, `memory`, `storage`,
    /// `telemetry`.
    pub fn sections_router(self: Arc<Self>) -> Router {
        Router::new()
            .route(
                "/providers",
                get(ProvidersPage::providers_list).post(ProvidersEditPage::providers_create),
            )
            .route("/providers/new", get(ProvidersEditPage::providers_new_form))
            .route(
                "/providers/{kind}",
                get(ProvidersEditPage::providers_detail)
                    .put(ProvidersEditPage::providers_update)
                    .delete(ProvidersEditPage::providers_remove),
            )
            .route(
                "/providers/{kind}/edit",
                get(ProvidersEditPage::providers_edit_form),
            )
            .route("/mcp", get(McpPage::mcp_list).post(McpEditPage::mcp_create))
            .route("/mcp/new", get(McpEditPage::mcp_new_form))
            .route(
                "/mcp/{name}",
                get(McpEditPage::mcp_detail)
                    .put(McpEditPage::mcp_update)
                    .delete(McpEditPage::mcp_remove),
            )
            .route("/mcp/{name}/edit", get(McpEditPage::mcp_edit_form))
            .route(
                "/auth",
                get(SectionEditPage::auth_get).put(SectionEditPage::auth_put),
            )
            .route(
                "/memory",
                get(SectionEditPage::memory_get).put(SectionEditPage::memory_put),
            )
            .route(
                "/storage",
                get(SectionEditPage::storage_get).put(SectionEditPage::storage_put),
            )
            .route(
                "/telemetry",
                get(SectionEditPage::telemetry_get).put(SectionEditPage::telemetry_put),
            )
            .with_state(self)
    }

    /// Serialize `body` and write it as the top-level `section` of the
    /// YAML file, validating the whole config first.
    async fn persist_section<T: serde::Serialize>(
        &self,
        section: &str,
        body: &T,
    ) -> Result<(), AdminError> {
        let value = serde_yaml::to_value(body)?;
        self.write_section(section, value).await?;
        Ok(())
    }
}

// NOTE: providers

#[derive(Template)]
#[template(path = "providers.html")]
struct ProvidersPage {
    providers: Vec<ProviderRow>,
}

impl ProvidersPage {
    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn providers_list(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(&cfg.providers).into_response());
        }
        let mut rows: Vec<ProviderRow> = cfg
            .providers
            .iter()
            .map(|(kind, p)| ProviderRow {
                kind: kind.as_str().to_string(),
                masked_key: mask_key(&p.api_key),
            })
            .collect();
        rows.sort_by(|a, b| a.kind.cmp(&b.kind));
        Ok(Html(ProvidersPage { providers: rows }.render()?).into_response())
    }
}

#[derive(Template)]
#[template(path = "providers_edit.html")]
struct ProvidersEditPage {
    action: String,
    is_new: bool,
    /// `None` on the create form, where the body names the kind.
    kind: Option<ProviderKind>,
    method: &'static str,
    yaml: String,
}

impl ProvidersEditPage {
    async fn providers_create(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
        EitherFormOrJson(body): EitherFormOrJson<ProviderCreateBody>,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if cfg.providers.contains_key(&body.kind) {
            return Err(AdminError::Conflict(format!(
                "provider '{}' already exists",
                body.kind.as_str()
            )));
        }
        let mut updated = cfg.providers.clone();
        updated.insert(body.kind, body.config.clone());
        store.persist_section("providers", &updated).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok((StatusCode::CREATED, Json(body.config)).into_response());
        }
        Ok(redirect_to("/admin/providers"))
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn providers_detail(
        State(store): State<Arc<ConfigStore>>,
        Path(kind): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let kind_enum =
            ProviderKind::parse(&kind).ok_or_else(|| AdminError::unknown_provider(&kind))?;
        let cfg = store.snapshot();
        let value = cfg
            .providers
            .get(&kind_enum)
            .ok_or(AdminError::NotFound)?
            .clone();
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(value).into_response());
        }
        Self::providers_edit_form(State(store), Path(kind)).await
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn providers_edit_form(
        State(store): State<Arc<ConfigStore>>,
        Path(kind): Path<String>,
    ) -> Result<Response, AdminError> {
        let kind_enum =
            ProviderKind::parse(&kind).ok_or_else(|| AdminError::unknown_provider(&kind))?;
        let cfg = store.snapshot();
        let value = cfg
            .providers
            .get(&kind_enum)
            .ok_or(AdminError::NotFound)?
            .clone();
        let yaml = serde_yaml::to_string(&value)?;
        Ok(Html(
            ProvidersEditPage {
                action: format!("/admin/providers/{kind}"),
                is_new: false,
                kind: Some(kind_enum),
                method: "put",
                yaml,
            }
            .render()?,
        )
        .into_response())
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn providers_new_form() -> Result<Response, AdminError> {
        let yaml = "kind: openai\napi_key: \n".to_string();
        Ok(Html(
            ProvidersEditPage {
                action: "/admin/providers".to_string(),
                is_new: true,
                kind: None,
                method: "post",
                yaml,
            }
            .render()?,
        )
        .into_response())
    }

    async fn providers_remove(
        State(store): State<Arc<ConfigStore>>,
        Path(kind): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let kind_enum =
            ProviderKind::parse(&kind).ok_or_else(|| AdminError::unknown_provider(&kind))?;
        let cfg = store.snapshot();
        let mut updated = cfg.providers.clone();
        if updated.remove(&kind_enum).is_none() {
            return Err(AdminError::NotFound);
        }
        store.persist_section("providers", &updated).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        Ok(redirect_to("/admin/providers"))
    }

    async fn providers_update(
        State(store): State<Arc<ConfigStore>>,
        Path(kind): Path<String>,
        fmt: ResponseFormat,
        EitherFormOrJson(body): EitherFormOrJson<ProviderConfig>,
    ) -> Result<Response, AdminError> {
        let kind_enum =
            ProviderKind::parse(&kind).ok_or_else(|| AdminError::unknown_provider(&kind))?;
        let cfg = store.snapshot();
        if !cfg.providers.contains_key(&kind_enum) {
            return Err(AdminError::NotFound);
        }
        let mut updated = cfg.providers.clone();
        updated.insert(kind_enum, body.clone());
        store.persist_section("providers", &updated).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(body).into_response());
        }
        Ok(redirect_to("/admin/providers"))
    }
}

struct ProviderRow {
    kind: String,
    masked_key: String,
}

#[derive(Deserialize)]
struct ProviderCreateBody {
    #[serde(flatten)]
    config: ProviderConfig,
    kind: ProviderKind,
}

// NOTE: mcp servers

#[derive(Template)]
#[template(path = "mcp.html")]
struct McpPage {
    servers: Vec<McpRow>,
}

impl McpPage {
    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn mcp_list(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(&cfg.mcp).into_response());
        }
        let mut rows: Vec<McpRow> = cfg
            .mcp
            .iter()
            .map(|(name, server)| McpRow::from_server(name, server))
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Html(McpPage { servers: rows }.render()?).into_response())
    }
}

#[derive(Template)]
#[template(path = "mcp_edit.html")]
struct McpEditPage {
    action: String,
    is_new: bool,
    method: &'static str,
    name: String,
    yaml: String,
}

impl McpEditPage {
    async fn mcp_create(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
        EitherFormOrJson(body): EitherFormOrJson<McpCreateBody>,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if cfg.mcp.contains_key(&body.name) {
            return Err(AdminError::Conflict(format!(
                "mcp server '{}' already exists",
                body.name
            )));
        }
        let mut updated = cfg.mcp.clone();
        updated.insert(body.name.clone(), body.server.clone());
        store.persist_section("mcp", &updated).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok((StatusCode::CREATED, Json(body.server)).into_response());
        }
        Ok(redirect_to("/admin/mcp"))
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn mcp_detail(
        State(store): State<Arc<ConfigStore>>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        let value = cfg.mcp.get(&name).ok_or(AdminError::NotFound)?.clone();
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(value).into_response());
        }
        Self::mcp_edit_form(State(store), Path(name)).await
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn mcp_edit_form(
        State(store): State<Arc<ConfigStore>>,
        Path(name): Path<String>,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        let value = cfg.mcp.get(&name).ok_or(AdminError::NotFound)?.clone();
        let yaml = serde_yaml::to_string(&value)?;
        Ok(Html(
            McpEditPage {
                action: format!("/admin/mcp/{name}"),
                is_new: false,
                method: "put",
                name,
                yaml,
            }
            .render()?,
        )
        .into_response())
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn mcp_new_form() -> Result<Response, AdminError> {
        let yaml = "name: \ntransport: stdio\ncommand: \nargs: []\n".to_string();
        Ok(Html(
            McpEditPage {
                action: "/admin/mcp".to_string(),
                is_new: true,
                method: "post",
                name: String::new(),
                yaml,
            }
            .render()?,
        )
        .into_response())
    }

    async fn mcp_remove(
        State(store): State<Arc<ConfigStore>>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        let mut updated = cfg.mcp.clone();
        if updated.remove(&name).is_none() {
            return Err(AdminError::NotFound);
        }
        store.persist_section("mcp", &updated).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        Ok(redirect_to("/admin/mcp"))
    }

    async fn mcp_update(
        State(store): State<Arc<ConfigStore>>,
        Path(name): Path<String>,
        fmt: ResponseFormat,
        EitherFormOrJson(body): EitherFormOrJson<McpServerConfig>,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if !cfg.mcp.contains_key(&name) {
            return Err(AdminError::NotFound);
        }
        let mut updated = cfg.mcp.clone();
        updated.insert(name.clone(), body.clone());
        store.persist_section("mcp", &updated).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(body).into_response());
        }
        Ok(redirect_to("/admin/mcp"))
    }
}

struct McpRow {
    name: String,
    summary: String,
}

impl McpRow {
    fn from_server(name: &str, server: &McpServerConfig) -> Self {
        let summary = match &server.transport {
            McpTransport::Http { url } => format!("http · {url}"),
            McpTransport::Sse { url } => format!("sse · {url}"),
            McpTransport::Stdio { args, command, .. } => {
                if args.is_empty() {
                    format!("stdio · {command}")
                } else {
                    format!("stdio · {command} {}", args.join(" "))
                }
            }
        };
        Self {
            name: name.to_string(),
            summary,
        }
    }
}

#[derive(Deserialize)]
struct McpCreateBody {
    name: String,
    #[serde(flatten)]
    server: McpServerConfig,
}

// NOTE: singleton config sections (auth, memory, storage, telemetry)
//
// Unlike providers/mcp these are single objects, not collections, so
// there is no list/new/delete — just view (GET) and replace (PUT). The
// edit form round-trips the section's own YAML slice; secrets render in
// cleartext, which is acceptable on an admin-only surface and matches
// the providers editor. None of these are hot-reloaded at runtime; the
// edit lands in `coulisse.yaml` and applies after restart.

/// One singleton section's editor page: where it posts, what it is
/// called, and the caveat shown under the title.
struct Section {
    action: &'static str,
    hint: &'static str,
    title: &'static str,
}

const AUTH_SECTION: Section = Section {
    action: "/admin/auth",
    hint: "Authentication for the /v1 proxy and /admin studio scopes. Secrets show in cleartext on this admin-only page. Changes take effect after restart.",
    title: "Auth",
};

const MEMORY_SECTION: Section = Section {
    action: "/admin/memory",
    hint: "Conversation storage and long-term user-state (recall/extraction) settings. Changes take effect after restart.",
    title: "Memory",
};

const STORAGE_SECTION: Section = Section {
    action: "/admin/storage",
    hint: "OpenAI-compatible file storage backend (filesystem or S3) and quotas. Changes take effect after restart.",
    title: "Storage",
};

const TELEMETRY_SECTION: Section = Section {
    action: "/admin/telemetry",
    hint: "Observability wiring: stderr logs, the SQLite mirror that drives this studio, and the optional OTLP exporter. Changes take effect after restart.",
    title: "Telemetry",
};

#[derive(Template)]
#[template(path = "section_edit.html")]
struct SectionEditPage {
    action: &'static str,
    hint: &'static str,
    title: &'static str,
    yaml: String,
}

impl SectionEditPage {
    /// The editor for one singleton section, seeded with its current
    /// YAML slice.
    fn of<T: serde::Serialize>(section: &Section, value: &T) -> Result<Self, AdminError> {
        Ok(Self {
            action: section.action,
            hint: section.hint,
            title: section.title,
            yaml: serde_yaml::to_string(value)?,
        })
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn auth_get(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(&cfg.auth).into_response());
        }
        SectionEditPage::of(&AUTH_SECTION, &cfg.auth)?.respond()
    }

    async fn auth_put(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
        EitherFormOrJson(body): EitherFormOrJson<auth::Config>,
    ) -> Result<Response, AdminError> {
        store.persist_section("auth", &body).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(body).into_response());
        }
        Ok(redirect_to("/admin/auth"))
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn memory_get(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(&cfg.memory).into_response());
        }
        SectionEditPage::of(&MEMORY_SECTION, &cfg.memory)?.respond()
    }

    async fn memory_put(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
        EitherFormOrJson(body): EitherFormOrJson<memory::MemoryYaml>,
    ) -> Result<Response, AdminError> {
        store.persist_section("memory", &body).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(body).into_response());
        }
        Ok(redirect_to("/admin/memory"))
    }

    fn respond(self) -> Result<Response, AdminError> {
        Ok(Html(self.render()?).into_response())
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn storage_get(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(&cfg.storage).into_response());
        }
        SectionEditPage::of(&STORAGE_SECTION, &cfg.storage)?.respond()
    }

    async fn storage_put(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
        EitherFormOrJson(body): EitherFormOrJson<storage::StorageYaml>,
    ) -> Result<Response, AdminError> {
        store.persist_section("storage", &body).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(body).into_response());
        }
        Ok(redirect_to("/admin/storage"))
    }

    #[allow(clippy::unused_async)] // axum handlers must return a future
    async fn telemetry_get(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
    ) -> Result<Response, AdminError> {
        let cfg = store.snapshot();
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(&cfg.telemetry).into_response());
        }
        SectionEditPage::of(&TELEMETRY_SECTION, &cfg.telemetry)?.respond()
    }

    async fn telemetry_put(
        State(store): State<Arc<ConfigStore>>,
        fmt: ResponseFormat,
        EitherFormOrJson(body): EitherFormOrJson<telemetry::Config>,
    ) -> Result<Response, AdminError> {
        store.persist_section("telemetry", &body).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(body).into_response());
        }
        Ok(redirect_to("/admin/telemetry"))
    }
}

#[derive(Debug, thiserror::Error)]
enum AdminError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Persist(#[from] ConfigPersistError),
    #[error("page render failed: {0}")]
    Render(#[from] askama::Error),
    #[error("could not serialize the section as YAML: {0}")]
    Serialize(#[from] serde_yaml::Error),
}

impl AdminError {
    fn unknown_provider(raw: &str) -> Self {
        Self::BadRequest(format!(
            "unknown provider '{raw}' (expected anthropic|cohere|deepseek|gemini|groq|openai)"
        ))
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::BadRequest(_) | Self::Persist(ConfigPersistError::Parse(_)) => {
                StatusCode::BAD_REQUEST
            }
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Persist(ConfigPersistError::Invalid(_)) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Persist(ConfigPersistError::Io(_)) | Self::Render(_) | Self::Serialize(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "config section endpoint failed");
        }
        (status, self.to_string()).into_response()
    }
}
