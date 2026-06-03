//! OpenAI-compatible `/v1/files` endpoints.
//!
//! All five standard methods:
//! - `POST   /v1/files`
//! - `GET    /v1/files`
//! - `GET    /v1/files/{id}`
//! - `GET    /v1/files/{id}/content`
//! - `DELETE /v1/files/{id}`

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde_json::json;
use storage::{FileObject, StorageError, Store};

pub fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/v1/files", post(upload).get(list))
        .route("/v1/files/{id}", get(get_metadata).delete(delete_file))
        .route("/v1/files/{id}/content", get(get_content))
        .with_state(store)
}

async fn upload(
    State(store): State<Arc<Store>>,
    mut multipart: Multipart,
) -> Result<Json<FileObject>, FilesError> {
    let mut file_bytes: Option<(String, String, Vec<u8>)> = None; // (filename, content_type, bytes)
    let mut purpose = String::from("assistants");

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| FilesError::BadRequest(format!("multipart error: {e}")))?
    {
        match field.name() {
            Some("purpose") => {
                purpose = field
                    .text()
                    .await
                    .map_err(|e| FilesError::BadRequest(format!("purpose read error: {e}")))?;
            }
            Some("file") => {
                let filename = field.file_name().unwrap_or("upload").to_string();
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| FilesError::BadRequest(format!("file read error: {e}")))?
                    .to_vec();
                file_bytes = Some((filename, content_type, bytes));
            }
            _ => {}
        }
    }

    let (filename, content_type, bytes) =
        file_bytes.ok_or_else(|| FilesError::BadRequest("missing 'file' field".into()))?;

    let meta = store
        .upload(&filename, &content_type, &purpose, "default", bytes)
        .await?;
    Ok(Json(meta))
}

async fn list(State(store): State<Arc<Store>>) -> Result<Json<serde_json::Value>, FilesError> {
    let files = store.list().await?;
    Ok(Json(json!({ "data": files, "object": "list" })))
}

async fn get_metadata(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<FileObject>, FilesError> {
    let meta = store.get_metadata(&id).await?;
    Ok(Json(meta))
}

async fn get_content(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Response, FilesError> {
    let (meta, bytes) = store.get_content(&id).await?;
    let content_type = meta.content_type.clone();
    let body = Bytes::from(bytes);
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            content_type
                .parse::<axum::http::HeaderValue>()
                .unwrap_or_else(|_| {
                    axum::http::HeaderValue::from_static("application/octet-stream")
                }),
        )],
        body,
    )
        .into_response())
}

async fn delete_file(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, FilesError> {
    store.delete(&id).await?;
    Ok(Json(json!({ "deleted": true, "id": id, "object": "file" })))
}

#[derive(Debug)]
enum FilesError {
    BadRequest(String),
    Storage(StorageError),
}

impl From<StorageError> for FilesError {
    fn from(err: StorageError) -> Self {
        Self::Storage(err)
    }
}

impl IntoResponse for FilesError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            Self::Storage(StorageError::NotFound(id)) => {
                (StatusCode::NOT_FOUND, format!("file '{id}' not found")).into_response()
            }
            Self::Storage(StorageError::FileTooLarge { size, limit }) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("file is {size} bytes; limit is {limit} bytes"),
            )
                .into_response(),
            Self::Storage(StorageError::UnsupportedContentType(ct)) => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!("content type '{ct}' is not allowed"),
            )
                .into_response(),
            Self::Storage(err) => {
                (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        }
    }
}
