use serde::{Deserialize, Serialize};

use crate::error::StorageError;

/// Top-level `storage:` block in `coulisse.yaml`.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema, Serialize)]
#[schemars(rename = "StorageConfig")]
pub struct StorageYaml {
    /// Storage backend. Defaults to `fs` if unset. The filesystem backend
    /// keeps blobs under `.coulisse/files` next to the config — there is no
    /// path knob; switch to `s3` for anything else.
    #[serde(default)]
    pub backend: BackendKind,
    /// Maximum bytes that may be stored per individual file.
    /// Defaults to no limit.
    #[serde(default)]
    pub max_file_bytes: Option<u64>,
    /// Maximum bytes that may be stored across all files (global quota).
    /// FIFO eviction removes the oldest file when the limit would be exceeded.
    /// Defaults to no limit.
    #[serde(default)]
    pub max_total_bytes: Option<u64>,
    /// S3-compatible backend options. Required when `backend: s3`.
    #[serde(default)]
    pub s3: Option<S3Config>,
}

#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    Fs,
    S3,
}

/// S3-compatible backend configuration.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct S3Config {
    pub bucket: String,
    /// Custom endpoint URL for `MinIO`, R2, or other S3-compatible services.
    #[serde(default)]
    pub endpoint_url: Option<EndpointUrl>,
    /// AWS region. Defaults to `us-east-1`.
    #[serde(default = "default_region")]
    pub region: String,
}

fn default_region() -> String {
    "us-east-1".to_string()
}

/// Endpoint of an S3-compatible service, passed verbatim to the AWS SDK
/// (which resolves and validates it when the client is built).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, PartialEq, Eq, Serialize)]
#[schemars(inline)]
#[serde(transparent)]
pub struct EndpointUrl(String);

impl EndpointUrl {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EndpointUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Quota settings extracted from `StorageYaml` for passing to `Store::open`.
#[derive(Clone, Debug, Default)]
pub struct QuotaConfig {
    pub max_file_bytes: Option<u64>,
    pub max_total_bytes: Option<u64>,
}

impl QuotaConfig {
    /// Reject a single file larger than `max_file_bytes`, when set.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::FileTooLarge` when `size` exceeds the limit.
    pub fn check_file_size(&self, size: u64) -> Result<(), StorageError> {
        if let Some(limit) = self.max_file_bytes
            && size > limit
        {
            return Err(StorageError::FileTooLarge { limit, size });
        }
        Ok(())
    }
}

impl From<&StorageYaml> for QuotaConfig {
    fn from(yaml: &StorageYaml) -> Self {
        Self {
            max_file_bytes: yaml.max_file_bytes,
            max_total_bytes: yaml.max_total_bytes,
        }
    }
}
