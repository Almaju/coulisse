use agents::AgentsError;
use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use backends::CallError;
use language::LanguageTagError;
use limits::LimitError;
use memory::MemoryError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("failed to bind server socket: {0}")]
    Bind(std::io::Error),
    #[error("server loop terminated: {0}")]
    Serve(std::io::Error),
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("invalid `metadata.language`: {0}")]
    Language(#[from] LanguageTagError),
    #[error("{0}")]
    Limit(#[from] LimitError),
    #[error("memory backend error: {0}")]
    Memory(#[from] MemoryError),
    #[error("{0}")]
    Agents(#[from] AgentsError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut retry_after: Option<u64> = None;
        let (status, kind) = match &self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            Self::Language(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            Self::Limit(LimitError::Database(_)) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "rate_limit_error")
            }
            Self::Limit(LimitError::Exceeded { retry_after: s, .. }) => {
                retry_after = Some(*s);
                (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
            }
            Self::Limit(LimitError::InvalidMetadata { .. }) => {
                (StatusCode::BAD_REQUEST, "invalid_request")
            }
            Self::Memory(_) => (StatusCode::INTERNAL_SERVER_ERROR, "memory_error"),
            Self::Agents(err) => match err {
                AgentsError::Backend(CallError::EmptyConversation) => {
                    (StatusCode::BAD_REQUEST, "invalid_request")
                }
                AgentsError::UnknownAgent(_) => (StatusCode::NOT_FOUND, "not_found"),
                _ => (StatusCode::BAD_GATEWAY, "upstream_error"),
            },
        };
        let body = Json(serde_json::json!({
            "error": {
                "message": self.to_string(),
                "type": kind,
            }
        }));
        let mut response = (status, body).into_response();
        if let Some(seconds) = retry_after
            && let Ok(value) = seconds.to_string().parse()
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}
