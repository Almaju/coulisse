use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::error_handling::HandleErrorLayer;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum_oidc::error::MiddlewareError;
use axum_oidc::{EmptyAdditionalClaims, OidcAuthLayer, OidcLoginLayer};
use judge::JudgeStoreError;
use memory::{MemoryError, UserId};
use telemetry::{TelemetryError, TurnId};
use time::Duration;
use tower::ServiceBuilder;
use tower_sessions::cookie::SameSite;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};
use uuid::Uuid;

use crate::auth::{StudioAuth, require_basic_auth};
use crate::state::StudioState;
use crate::templates::{ConversationPage, EventsFragment, ExperimentsPage, UsersPage};
use crate::views::{ScoresPanel, event_rows, message_rows};

/// Build the studio router. Auth layers (Basic or OIDC) are attached here
/// based on `state.auth` so the cli only needs to mount the result under
/// its public path.
pub fn router(state: Arc<StudioState>) -> Router {
    let routes = Router::new()
        .route("/", get(users))
        .route("/experiments", get(experiments))
        .route("/users/{user_id}", get(conversation))
        .route("/users/{user_id}/turns/{turn_id}/events", get(turn_events))
        .with_state(Arc::clone(&state));

    match state.auth.as_ref() {
        None => routes,
        Some(StudioAuth::Basic(_)) => {
            routes.route_layer(from_fn_with_state(state, require_basic_auth))
        }
        Some(StudioAuth::Oidc(runtime)) => {
            // Session → auth (reads session, sets extensions) → login
            // (forces redirect when no valid ID token). `.layer()` calls
            // are applied outermost-last; session must wrap everything so
            // the OIDC layers find it in request extensions.
            // `HandleErrorLayer` converts the OIDC middlewares'
            // `MiddlewareError` into axum-compatible `Infallible`
            // responses.
            let session = SessionManagerLayer::new(MemoryStore::default())
                .with_same_site(SameSite::Lax)
                .with_expiry(Expiry::OnInactivity(Duration::hours(8)));
            let oidc_login = ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_oidc_error))
                .layer(OidcLoginLayer::<EmptyAdditionalClaims>::new());
            let oidc_auth = ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_oidc_error))
                .layer(OidcAuthLayer::<EmptyAdditionalClaims>::new(
                    runtime.client.clone(),
                ));
            routes.layer(oidc_login).layer(oidc_auth).layer(session)
        }
    }
}

async fn handle_oidc_error(err: MiddlewareError) -> Response {
    err.into_response()
}

async fn users(State(state): State<Arc<StudioState>>) -> Result<Html<String>, StudioError> {
    let users = state
        .memory
        .list_user_summaries()
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    render(UsersPage { users })
}

async fn conversation(
    State(state): State<Arc<StudioState>>,
    Path(user_id): Path<String>,
) -> Result<Html<String>, StudioError> {
    let user_id = parse_user_id(&user_id)?;
    let um = state.memory.for_user(user_id);
    let messages = um.messages().await?;
    let tool_calls = state.telemetry.tool_calls_for_user(user_id).await?;
    let memories = um.memories().await?.into_iter().map(Into::into).collect();
    let scores = ScoresPanel::build(state.judges.scores(user_id).await?);
    render(ConversationPage {
        memories,
        messages: message_rows(messages, tool_calls),
        scores,
        user_id: user_id.0.to_string(),
    })
}

async fn experiments(State(state): State<Arc<StudioState>>) -> Result<Html<String>, StudioError> {
    let mut rows: Vec<crate::views::ExperimentRow> = Vec::with_capacity(state.experiments.len());
    for exp in &state.experiments {
        let mut scores: std::collections::HashMap<String, (f32, u32)> =
            std::collections::HashMap::new();
        if let Some(metric) = exp.metric.as_deref()
            && let Some((judge, criterion)) = metric.split_once('.')
        {
            let window = exp.bandit_window_seconds.unwrap_or(7 * 24 * 60 * 60);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let since = now.saturating_sub(window);
            for s in state
                .judges
                .mean_scores_by_agent(judge, criterion, since)
                .await?
            {
                scores.insert(s.agent_name, (s.mean, s.samples));
            }
        }
        rows.push(crate::views::ExperimentRow::build(exp, &scores));
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    render(ExperimentsPage { experiments: rows })
}

async fn turn_events(
    State(state): State<Arc<StudioState>>,
    Path((user_id, turn_id)): Path<(String, String)>,
) -> Result<Html<String>, StudioError> {
    let user_id = parse_user_id(&user_id)?;
    let turn_id = parse_turn_id(&turn_id)?;
    let events = state.telemetry.fetch_turn(user_id, turn_id).await?;
    render(EventsFragment {
        rows: event_rows(events),
    })
}

fn render<T: Template>(tpl: T) -> Result<Html<String>, StudioError> {
    Ok(Html(tpl.render()?))
}

fn parse_user_id(raw: &str) -> Result<UserId, StudioError> {
    Uuid::parse_str(raw)
        .map(UserId::from)
        .map_err(|_| StudioError::InvalidUserId)
}

fn parse_turn_id(raw: &str) -> Result<TurnId, StudioError> {
    Uuid::parse_str(raw)
        .map(TurnId)
        .map_err(|_| StudioError::InvalidTurnId)
}

#[derive(Debug)]
enum StudioError {
    InvalidTurnId,
    InvalidUserId,
    Judge(JudgeStoreError),
    Memory(MemoryError),
    Render(askama::Error),
    Telemetry(TelemetryError),
}

impl From<JudgeStoreError> for StudioError {
    fn from(err: JudgeStoreError) -> Self {
        Self::Judge(err)
    }
}

impl From<MemoryError> for StudioError {
    fn from(err: MemoryError) -> Self {
        Self::Memory(err)
    }
}

impl From<TelemetryError> for StudioError {
    fn from(err: TelemetryError) -> Self {
        Self::Telemetry(err)
    }
}

impl From<askama::Error> for StudioError {
    fn from(err: askama::Error) -> Self {
        Self::Render(err)
    }
}

impl IntoResponse for StudioError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidTurnId => (
                StatusCode::BAD_REQUEST,
                "turn_id must be a valid UUID".to_string(),
            ),
            Self::InvalidUserId => (
                StatusCode::BAD_REQUEST,
                "user_id must be a valid UUID".to_string(),
            ),
            Self::Judge(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Self::Memory(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Self::Render(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            Self::Telemetry(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        (status, message).into_response()
    }
}
