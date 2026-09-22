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
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use coulisse_core::UserId;
use serde_json::json;
use storage::{FileId, FileObject, InvalidFileId, StorageError, Store, Upload};

/// The `/v1/files` API over one file store.
pub struct FilesApi {
    pub store: Arc<Store>,
}

impl FilesApi {
    pub fn router(self) -> Router {
        Router::new()
            .route("/v1/files", post(upload).get(list))
            .route("/v1/files/{id}", get(get_metadata).delete(delete_file))
            .route("/v1/files/{id}/content", get(get_content))
            .with_state(self.store)
    }
}

/// The multipart fields of a `POST /v1/files` body as they arrive.
#[derive(Default)]
struct UploadForm {
    file: Option<UploadedFile>,
    purpose: Option<String>,
}

struct UploadedFile {
    bytes: Vec<u8>,
    content_type: String,
    filename: String,
}

impl UploadForm {
    async fn read(mut multipart: Multipart) -> Result<Self, FilesError> {
        let mut form = Self::default();
        while let Some(field) =
            multipart
                .next_field()
                .await
                .map_err(|source| FilesError::Multipart {
                    context: "multipart error",
                    source,
                })?
        {
            match field.name() {
                Some("purpose") => {
                    form.purpose =
                        Some(field.text().await.map_err(|source| FilesError::Multipart {
                            context: "purpose read error",
                            source,
                        })?);
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
                        .map_err(|source| FilesError::Multipart {
                            context: "file read error",
                            source,
                        })?
                        .to_vec();
                    form.file = Some(UploadedFile {
                        bytes,
                        content_type,
                        filename,
                    });
                }
                _ => {}
            }
        }
        Ok(form)
    }

    fn into_upload(self) -> Result<Upload, FilesError> {
        let file = self.file.ok_or(FilesError::MissingFile)?;
        Ok(Upload {
            bytes: file.bytes,
            content_type: file.content_type,
            filename: file.filename,
            purpose: self.purpose.unwrap_or_else(|| "assistants".to_string()),
            user_id: UserId::from_string("default"),
        })
    }
}

async fn upload(
    State(store): State<Arc<Store>>,
    multipart: Multipart,
) -> Result<Json<FileObject>, FilesError> {
    let upload = UploadForm::read(multipart).await?.into_upload()?;
    let meta = store.upload(upload).await?;
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
    let id: FileId = id.parse()?;
    let meta = store.get_metadata(&id).await?;
    Ok(Json(meta))
}

async fn get_content(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Response, FilesError> {
    let id: FileId = id.parse()?;
    let (meta, bytes) = store.get_content(&id).await?;
    let content_type = HeaderValue::from_str(&meta.content_type)
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    let body = Bytes::from(bytes);
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], body).into_response())
}

async fn delete_file(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, FilesError> {
    let file_id: FileId = id.parse()?;
    store.delete(&file_id).await?;
    Ok(Json(json!({ "deleted": true, "id": id, "object": "file" })))
}

#[derive(Debug, thiserror::Error)]
enum FilesError {
    #[error("{0}")]
    InvalidId(#[from] InvalidFileId),
    #[error("missing 'file' field")]
    MissingFile,
    #[error("{context}: {source}")]
    Multipart {
        context: &'static str,
        #[source]
        source: axum::extract::multipart::MultipartError,
    },
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl IntoResponse for FilesError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidId(
                InvalidFileId::MissingPrefix(id) | InvalidFileId::Uuid { raw: id, .. },
            ) => (StatusCode::NOT_FOUND, format!("file '{id}' not found")).into_response(),
            Self::MissingFile | Self::Multipart { .. } => {
                (StatusCode::BAD_REQUEST, self.to_string()).into_response()
            }
            Self::Storage(StorageError::NotFound(id)) => {
                (StatusCode::NOT_FOUND, format!("file '{id}' not found")).into_response()
            }
            Self::Storage(StorageError::FileTooLarge { limit, size }) => (
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
                tracing::error!(error = %err, "file store request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
            }
        }
    }
}
