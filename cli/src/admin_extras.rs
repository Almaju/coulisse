//! Cli-owned admin endpoints for sections that don't have a feature
//! crate admin module today: providers and MCP servers. Same shape as
//! the per-feature admin routers — content negotiation, JSON/YAML/form
//! body parsing, write-through to `coulisse.yaml`. Edits land in the
//! file and refresh the admin display via the `ConfigStore` snapshot;
//! the runtime that consumes them (`providers::Providers` and
//! `mcp::McpServers`) is built at boot and still requires a restart
//! to swap.

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use coulisse_core::{ConfigPersistError, ConfigPersister, EitherFormOrJson, ResponseFormat};
use mcp::McpServerConfig;
use providers::{ProviderConfig, ProviderKind};
use serde::Deserialize;

use crate::config_store::ConfigStore;

#[derive(Clone)]
struct State_ {
    store: Arc<ConfigStore>,
}

pub fn router(store: Arc<ConfigStore>) -> Router {
    let state = State_ { store };
    Router::new()
        .route("/providers", get(providers_list).post(providers_create))
        .route("/providers/new", get(providers_new_form))
        .route(
            "/providers/{kind}",
            get(providers_detail)
                .put(providers_update)
                .delete(providers_remove),
        )
        .route("/providers/{kind}/edit", get(providers_edit_form))
        .route("/mcp", get(mcp_list).post(mcp_create))
        .route("/mcp/new", get(mcp_new_form))
        .route(
            "/mcp/{name}",
            get(mcp_detail).put(mcp_update).delete(mcp_remove),
        )
        .route("/mcp/{name}/edit", get(mcp_edit_form))
        .with_state(state)
}

// ---- providers --------------------------------------------------------

#[derive(Template)]
#[template(path = "providers.html")]
struct ProvidersPage {
    providers: Vec<ProviderRow>,
}

#[derive(Template)]
#[template(path = "providers_edit.html")]
struct ProvidersEditPage {
    action: String,
    is_new: bool,
    kind: String,
    method: &'static str,
    yaml: String,
}

struct ProviderRow {
    kind: String,
    masked_key: String,
}

async fn providers_list(
    State(state): State<State_>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let cfg = state.store.snapshot();
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

async fn providers_detail(
    State(state): State<State_>,
    Path(kind): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let kind_enum = parse_provider_kind(&kind)?;
    let cfg = state.store.snapshot();
    let value = cfg
        .providers
        .get(&kind_enum)
        .ok_or(AdminError::NotFound)?
        .clone();
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(value).into_response());
    }
    // No bespoke detail page; fall back to the edit form.
    providers_edit_form(State(state), Path(kind)).await
}

#[derive(Deserialize)]
struct ProviderCreateBody {
    kind: ProviderKind,
    #[serde(flatten)]
    config: ProviderConfig,
}

async fn providers_create(
    State(state): State<State_>,
    fmt: ResponseFormat,
    EitherFormOrJson(body): EitherFormOrJson<ProviderCreateBody>,
) -> Result<Response, AdminError> {
    let cfg = state.store.snapshot();
    if cfg.providers.contains_key(&body.kind) {
        return Err(AdminError::Conflict(format!(
            "provider '{}' already exists",
            body.kind.as_str()
        )));
    }
    let mut updated = cfg.providers.clone();
    updated.insert(body.kind, body.config.clone());
    persist_providers(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok((StatusCode::CREATED, Json(body.config)).into_response());
    }
    redirect("/admin/providers")
}

async fn providers_update(
    State(state): State<State_>,
    Path(kind): Path<String>,
    fmt: ResponseFormat,
    EitherFormOrJson(body): EitherFormOrJson<ProviderConfig>,
) -> Result<Response, AdminError> {
    let kind_enum = parse_provider_kind(&kind)?;
    let cfg = state.store.snapshot();
    if !cfg.providers.contains_key(&kind_enum) {
        return Err(AdminError::NotFound);
    }
    let mut updated = cfg.providers.clone();
    updated.insert(kind_enum, body.clone());
    persist_providers(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(body).into_response());
    }
    redirect("/admin/providers")
}

async fn providers_remove(
    State(state): State<State_>,
    Path(kind): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let kind_enum = parse_provider_kind(&kind)?;
    let cfg = state.store.snapshot();
    let mut updated = cfg.providers.clone();
    if updated.remove(&kind_enum).is_none() {
        return Err(AdminError::NotFound);
    }
    persist_providers(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    redirect("/admin/providers")
}

async fn providers_edit_form(
    State(state): State<State_>,
    Path(kind): Path<String>,
) -> Result<Response, AdminError> {
    let kind_enum = parse_provider_kind(&kind)?;
    let cfg = state.store.snapshot();
    let value = cfg
        .providers
        .get(&kind_enum)
        .ok_or(AdminError::NotFound)?
        .clone();
    let yaml =
        serde_yaml::to_string(&value).map_err(|err| AdminError::Internal(err.to_string()))?;
    Ok(Html(
        ProvidersEditPage {
            action: format!("/admin/providers/{kind}"),
            is_new: false,
            kind,
            method: "put",
            yaml,
        }
        .render()?,
    )
    .into_response())
}

async fn providers_new_form() -> Result<Response, AdminError> {
    let yaml = "kind: openai\napi_key: \n".to_string();
    Ok(Html(
        ProvidersEditPage {
            action: "/admin/providers".to_string(),
            is_new: true,
            kind: String::new(),
            method: "post",
            yaml,
        }
        .render()?,
    )
    .into_response())
}

async fn persist_providers(
    state: &State_,
    providers: HashMap<ProviderKind, ProviderConfig>,
) -> Result<(), AdminError> {
    let value =
        serde_yaml::to_value(&providers).map_err(|err| AdminError::Internal(err.to_string()))?;
    state
        .store
        .write_section("providers", value)
        .await
        .map_err(AdminError::from)
}

fn parse_provider_kind(s: &str) -> Result<ProviderKind, AdminError> {
    ProviderKind::parse(s).ok_or_else(|| {
        AdminError::BadRequest(format!(
            "unknown provider '{s}' (expected anthropic|cohere|deepseek|gemini|groq|openai)"
        ))
    })
}

fn mask_key(key: &str) -> String {
    if key.len() > 4 {
        format!("····{}", &key[key.len() - 4..])
    } else {
        "····".to_string()
    }
}

// ---- mcp servers ------------------------------------------------------

#[derive(Template)]
#[template(path = "mcp.html")]
struct McpPage {
    servers: Vec<McpRow>,
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

struct McpRow {
    name: String,
    summary: String,
}

async fn mcp_list(
    State(state): State<State_>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let cfg = state.store.snapshot();
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(&cfg.mcp).into_response());
    }
    let mut rows: Vec<McpRow> = cfg
        .mcp
        .iter()
        .map(|(name, server)| McpRow {
            name: name.clone(),
            summary: mcp_summary(server),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Html(McpPage { servers: rows }.render()?).into_response())
}

async fn mcp_detail(
    State(state): State<State_>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let cfg = state.store.snapshot();
    let value = cfg.mcp.get(&name).ok_or(AdminError::NotFound)?.clone();
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(value).into_response());
    }
    mcp_edit_form(State(state), Path(name)).await
}

#[derive(Deserialize)]
struct McpCreateBody {
    name: String,
    #[serde(flatten)]
    server: McpServerConfig,
}

async fn mcp_create(
    State(state): State<State_>,
    fmt: ResponseFormat,
    EitherFormOrJson(body): EitherFormOrJson<McpCreateBody>,
) -> Result<Response, AdminError> {
    let cfg = state.store.snapshot();
    if cfg.mcp.contains_key(&body.name) {
        return Err(AdminError::Conflict(format!(
            "mcp server '{}' already exists",
            body.name
        )));
    }
    let mut updated = cfg.mcp.clone();
    updated.insert(body.name.clone(), body.server.clone());
    persist_mcp(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok((StatusCode::CREATED, Json(body.server)).into_response());
    }
    redirect("/admin/mcp")
}

async fn mcp_update(
    State(state): State<State_>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
    EitherFormOrJson(body): EitherFormOrJson<McpServerConfig>,
) -> Result<Response, AdminError> {
    let cfg = state.store.snapshot();
    if !cfg.mcp.contains_key(&name) {
        return Err(AdminError::NotFound);
    }
    let mut updated = cfg.mcp.clone();
    updated.insert(name.clone(), body.clone());
    persist_mcp(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(body).into_response());
    }
    redirect("/admin/mcp")
}

async fn mcp_remove(
    State(state): State<State_>,
    Path(name): Path<String>,
    fmt: ResponseFormat,
) -> Result<Response, AdminError> {
    let cfg = state.store.snapshot();
    let mut updated = cfg.mcp.clone();
    if updated.remove(&name).is_none() {
        return Err(AdminError::NotFound);
    }
    persist_mcp(&state, updated).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    redirect("/admin/mcp")
}

async fn mcp_edit_form(
    State(state): State<State_>,
    Path(name): Path<String>,
) -> Result<Response, AdminError> {
    let cfg = state.store.snapshot();
    let value = cfg.mcp.get(&name).ok_or(AdminError::NotFound)?.clone();
    let yaml =
        serde_yaml::to_string(&value).map_err(|err| AdminError::Internal(err.to_string()))?;
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

async fn persist_mcp(
    state: &State_,
    mcp: HashMap<String, McpServerConfig>,
) -> Result<(), AdminError> {
    let value = serde_yaml::to_value(&mcp).map_err(|err| AdminError::Internal(err.to_string()))?;
    state
        .store
        .write_section("mcp", value)
        .await
        .map_err(AdminError::from)
}

fn mcp_summary(server: &McpServerConfig) -> String {
    match server {
        McpServerConfig::Http { url } => format!("http · {url}"),
        McpServerConfig::Stdio { command, args, .. } => {
            if args.is_empty() {
                format!("stdio · {command}")
            } else {
                format!("stdio · {command} {}", args.join(" "))
            }
        }
    }
}

// ---- shared error / redirect ----------------------------------------

fn redirect(to: &str) -> Result<Response, AdminError> {
    let mut resp = (StatusCode::SEE_OTHER, [("location", to)]).into_response();
    resp.headers_mut().insert(
        "hx-redirect",
        axum::http::HeaderValue::from_str(to).expect("valid header value"),
    );
    Ok(resp)
}

#[derive(Debug)]
enum AdminError {
    BadRequest(String),
    Conflict(String),
    Internal(String),
    InvalidConfig(String),
    NotFound,
    Render(askama::Error),
}

impl From<askama::Error> for AdminError {
    fn from(err: askama::Error) -> Self {
        Self::Render(err)
    }
}

impl From<ConfigPersistError> for AdminError {
    fn from(err: ConfigPersistError) -> Self {
        match err {
            ConfigPersistError::Invalid(m) | ConfigPersistError::Parse(m) => Self::InvalidConfig(m),
            ConfigPersistError::Io(m) => Self::Internal(m),
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Conflict(m) => (StatusCode::CONFLICT, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            Self::InvalidConfig(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, msg).into_response()
    }
}
