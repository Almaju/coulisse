//! Studio admin surface for self-issued API tokens: list with spend, mint
//! (revealing the secret once), and revoke. Mounted by cli under
//! `/admin/tokens` and wrapped in the admin auth scope, exactly like every
//! other feature crate's admin router.

use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use coulisse_core::{EitherFormOrJson, ResponseFormat, redirect_to};
use serde::Deserialize;
use thiserror::Error;

use crate::token::{Budget, BudgetParseError, StoreError, TokenId, TokenRecord, TokenStore};

/// Mount the token admin routes against the shared token store.
pub fn router(store: Arc<TokenStore>) -> Router {
    Router::new()
        .route("/tokens", get(list).post(create))
        .route("/tokens/{id}", axum::routing::delete(revoke))
        .with_state(store)
}

async fn list(State(store): State<Arc<TokenStore>>) -> Result<Response, AdminError> {
    let tokens = views(store.list().await?);
    Ok(Html(TokensPage { tokens }.render()?).into_response())
}

/// Form/JSON body for minting a token. `budget_usd` rides as a string so an
/// empty form field deserializes cleanly to "no amount" rather than failing
/// f64 parsing; the handler trims and parses it.
#[derive(Debug, Deserialize)]
struct CreateForm {
    #[serde(default = "default_kind")]
    budget_kind: String,
    #[serde(default)]
    budget_usd: Option<String>,
    label: String,
    principal: String,
}

async fn create(
    State(store): State<Arc<TokenStore>>,
    fmt: ResponseFormat,
    EitherFormOrJson(form): EitherFormOrJson<CreateForm>,
) -> Result<Response, AdminError> {
    if form.label.trim().is_empty() || form.principal.trim().is_empty() {
        return Err(AdminError::MissingField);
    }
    let amount = form
        .budget_usd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::parse::<f64>)
        .transpose()
        .map_err(|_| AdminError::Budget(BudgetParseError::NonPositiveLimit))?;
    let budget = Budget::from_parts(&form.budget_kind, amount)?;
    let minted = store
        .mint(form.label.trim(), form.principal.trim(), budget)
        .await?;

    if matches!(fmt, ResponseFormat::Json) {
        return Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": minted.id,
                "secret": minted.secret,
            })),
        )
            .into_response());
    }
    // The secret can never be shown again — render the reveal fragment
    // rather than redirecting to a detail page that couldn't display it.
    Ok(Html(
        SecretReveal {
            label: form.label.trim().to_string(),
            secret: minted.secret,
        }
        .render()?,
    )
    .into_response())
}

async fn revoke(
    State(store): State<Arc<TokenStore>>,
    fmt: ResponseFormat,
    Path(id): Path<String>,
) -> Result<Response, AdminError> {
    let token_id = TokenId::parse(&id).map_err(|_| AdminError::BadId)?;
    let revoked = store.revoke(token_id).await?;
    if matches!(fmt, ResponseFormat::Json) {
        return Ok(Json(serde_json::json!({ "revoked": revoked })).into_response());
    }
    Ok(redirect_to("/admin/tokens"))
}

/// Display-ready projection of a [`TokenRecord`].
struct TokenView {
    budget: String,
    id: String,
    label: String,
    period_spend: String,
    principal: String,
    revoked: bool,
    spend: String,
}

fn views(records: Vec<TokenRecord>) -> Vec<TokenView> {
    records
        .into_iter()
        .map(|r| TokenView {
            budget: r.budget.describe(),
            id: r.id.to_string(),
            label: r.label.clone(),
            period_spend: format!("${:.2}", r.period_spend_usd()),
            principal: r.principal.clone(),
            revoked: r.is_revoked(),
            spend: format!("${:.2}", r.spend_usd()),
        })
        .collect()
}

fn default_kind() -> String {
    "unlimited".to_string()
}

#[derive(Template)]
#[template(path = "tokens.html")]
struct TokensPage {
    tokens: Vec<TokenView>,
}

#[derive(Template)]
#[template(path = "token_created.html")]
struct SecretReveal {
    label: String,
    secret: String,
}

#[derive(Debug, Error)]
enum AdminError {
    #[error("token id is not a valid uuid")]
    BadId,
    #[error(transparent)]
    Budget(#[from] BudgetParseError),
    #[error("label and principal are required")]
    MissingField,
    #[error("failed to render token page: {0}")]
    Render(#[from] askama::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::BadId | Self::Budget(_) | Self::MissingField => StatusCode::BAD_REQUEST,
            Self::Render(_) | Self::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}
