use serde::{Deserialize, Serialize};

/// Top-level `storage:` block in `coulisse.yaml`.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema, Serialize)]
#[schemars(rename = "StorageConfig")]
pub struct StorageYaml {
    /// Storage backend. Defaults to `fs` if unset.
    #[serde(default)]
    pub backend: BackendKind,
    /// File-system backend options. Ignored when `backend: s3`.
    #[serde(default)]
    pub fs: FsConfig,
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

/// File-system backend configuration.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct FsConfig {
    /// Directory where blobs are stored. Defaults to `./coulisse-files`.
    #[serde(default = "default_fs_path")]
    pub path: std::path::PathBuf,
}

impl Default for FsConfig {
    fn default() -> Self {
        Self {
            path: default_fs_path(),
        }
    }
}

fn default_fs_path() -> std::path::PathBuf {
    std::path::PathBuf::from("./coulisse-files")
}

/// S3-compatible backend configuration.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct S3Config {
    pub bucket: String,
    /// Custom endpoint URL for `MinIO`, R2, or other S3-compatible services.
    #[serde(default)]
    pub endpoint_url: Option<String>,
    /// AWS region. Defaults to `us-east-1`.
    #[serde(default = "default_region")]
    pub region: String,
}

fn default_region() -> String {
    "us-east-1".to_string()
}

/// Quota settings extracted from `StorageYaml` for passing to `Store::open`.
#[derive(Clone, Debug, Default)]
pub struct QuotaConfig {
    pub max_file_bytes: Option<u64>,
    pub max_total_bytes: Option<u64>,
}

impl From<&StorageYaml> for QuotaConfig {
    fn from(yaml: &StorageYaml) -> Self {
        Self {
            max_file_bytes: yaml.max_file_bytes,
            max_total_bytes: yaml.max_total_bytes,
        }
    }
}
