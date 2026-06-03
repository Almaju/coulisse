//! OpenAI-compatible file storage.
//!
//! Provides `POST /v1/files`, `GET /v1/files`, `GET /v1/files/:id`,
//! `GET /v1/files/:id/content`, and `DELETE /v1/files/:id` — all matching
//! the `OpenAI` Files API shape so any OpenAI-compatible client works
//! without modification.

pub mod backend;
pub mod config;
pub mod error;
pub mod mime;
pub mod s3;
pub mod store;

pub use backend::{BlobBackend, FsBackend};
pub use config::{BackendKind, QuotaConfig, S3Config, StorageYaml};
pub use error::StorageError;
pub use store::{FileObject, Store};

#[cfg(feature = "s3")]
pub use s3::S3Backend;
