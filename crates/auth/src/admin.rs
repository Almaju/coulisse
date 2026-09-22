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

use crate::token::{
    Budget, BudgetKind, BudgetParseError, NewToken, StoreError, TokenId, TokenRecord, TokenSecret,
    TokenStore,
};

/// Router state for the token admin pages: the shared token store.
#[derive(Clone)]
pub struct TokenAdmin {
    store: Arc<TokenStore>,
}

impl TokenAdmin {
    #[must_use]
    pub fn new(store: Arc<TokenStore>) -> Self {
        Self { store }
    }

    /// Mount the token admin routes against the shared token store.
    pub fn router(self) -> Router {
        Router::new()
            .route("/tokens", get(Self::list).post(Self::create))
            .route("/tokens/{id}", axum::routing::delete(Self::revoke))
            .with_state(self)
    }

    async fn create(
        State(admin): State<Self>,
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
            .map_err(AdminError::BudgetAmount)?;
        let budget = Budget::from_parts(form.budget_kind, amount)?;
        let minted = admin
            .store
            .mint(NewToken {
                budget,
                label: form.label.trim().to_string(),
                principal: form.principal.trim().to_string(),
            })
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

    async fn list(State(admin): State<Self>) -> Result<Response, AdminError> {
        let tokens = admin
            .store
            .list()
            .await?
            .into_iter()
            .map(TokenView::from)
            .collect();
        Ok(Html(TokensPage { tokens }.render()?).into_response())
    }

    async fn revoke(
        State(admin): State<Self>,
        fmt: ResponseFormat,
        Path(id): Path<String>,
    ) -> Result<Response, AdminError> {
        let token_id = id.parse::<TokenId>().map_err(AdminError::BadId)?;
        let revoked = admin.store.revoke(token_id).await?;
        if matches!(fmt, ResponseFormat::Json) {
            return Ok(Json(serde_json::json!({ "revoked": revoked })).into_response());
        }
        Ok(redirect_to("/admin/tokens"))
    }
}

/// Form/JSON body for minting a token. `budget_usd` rides as a string so an
/// empty form field deserializes cleanly to "no amount" rather than failing
/// f64 parsing; the handler trims and parses it.
#[derive(Debug, Deserialize)]
struct CreateForm {
    #[serde(default)]
    budget_kind: BudgetKind,
    #[serde(default)]
    budget_usd: Option<String>,
    label: String,
    principal: String,
}

/// Display-ready projection of a [`TokenRecord`].
struct TokenView {
    budget: String,
    id: TokenId,
    label: String,
    period_spend: String,
    principal: String,
    revoked: bool,
    spend: String,
}

impl From<TokenRecord> for TokenView {
    fn from(record: TokenRecord) -> Self {
        let period_spend = format!("${:.2}", record.period_spend_usd());
        let revoked = record.is_revoked();
        let spend = format!("${:.2}", record.spend_usd());
        Self {
            budget: record.budget.describe(),
            id: record.id,
            label: record.label,
            period_spend,
            principal: record.principal,
            revoked,
            spend,
        }
    }
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
    secret: TokenSecret,
}

#[derive(Debug, Error)]
enum AdminError {
    #[error("token id is not a valid uuid: {0}")]
    BadId(#[source] uuid::Error),
    #[error(transparent)]
    Budget(#[from] BudgetParseError),
    #[error("budget amount is not a number: {0}")]
    BudgetAmount(#[source] std::num::ParseFloatError),
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
            Self::BadId(_) | Self::Budget(_) | Self::BudgetAmount(_) | Self::MissingField => {
                StatusCode::BAD_REQUEST
            }
            Self::Render(_) | Self::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}
