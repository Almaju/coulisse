use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::TokenCount;

/// Runtime configuration for the memory subsystem. Every field is optional in
/// YAML — omitting the whole `memory:` block yields in-process defaults
/// suitable for development (hash embedder, SQLite file in the working dir,
/// no auto-extraction).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemoryConfig {
    #[serde(default)]
    pub backend: BackendConfig,
    #[serde(default = "default_context_budget")]
    pub context_budget: TokenCount,
    #[serde(default)]
    pub embedder: EmbedderConfig,
    #[serde(default)]
    pub extractor: Option<ExtractorConfig>,
    #[serde(default = "default_memory_budget_fraction")]
    pub memory_budget_fraction: f32,
    #[serde(default = "default_recall_k")]
    pub recall_k: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: BackendConfig::default(),
            context_budget: default_context_budget(),
            embedder: EmbedderConfig::default(),
            extractor: None,
            memory_budget_fraction: default_memory_budget_fraction(),
            recall_k: default_recall_k(),
        }
    }
}

/// Where memory data is stored. `InMemory` is an ephemeral SQLite database
/// that evaporates with the process — useful for tests and short-lived demos.
/// `Sqlite` is a file-backed database; for Docker deployments, point `path`
/// at a volume-mounted directory so data survives container restarts.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendConfig {
    InMemory,
    Sqlite {
        #[serde(default = "default_sqlite_path")]
        path: PathBuf,
    },
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self::Sqlite {
            path: default_sqlite_path(),
        }
    }
}

/// Which embedder turns text into vectors. The `hash` provider is a
/// deterministic bag-of-words embedder suitable only for tests and
/// air-gapped development — it has no semantic understanding.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum EmbedderConfig {
    Hash {
        #[serde(default = "default_hash_dims")]
        dims: usize,
    },
    Openai {
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default = "default_openai_model")]
        model: String,
    },
    Voyage {
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default = "default_voyage_model")]
        model: String,
    },
}

impl Default for EmbedderConfig {
    fn default() -> Self {
        Self::Hash {
            dims: default_hash_dims(),
        }
    }
}

impl EmbedderConfig {
    /// Stable identifier used in the `embedding_model` column so recall can
    /// refuse vectors written by a different embedder.
    pub fn model_id(&self) -> String {
        match self {
            Self::Hash { dims } => format!("hash-{dims}"),
            Self::Openai { model, .. } => format!("openai:{model}"),
            Self::Voyage { model, .. } => format!("voyage:{model}"),
        }
    }
}

/// Which completion model extracts durable facts from each exchange. When
/// `None`, auto-extraction is off and the `memories` table is only written
/// via explicit API calls.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExtractorConfig {
    #[serde(default = "default_dedup_threshold")]
    pub dedup_threshold: f32,
    #[serde(default = "default_extractor_max_facts")]
    pub max_facts_per_turn: usize,
    pub model: String,
    pub provider: String,
}

fn default_context_budget() -> TokenCount {
    TokenCount(8_000)
}

fn default_dedup_threshold() -> f32 {
    0.9
}

fn default_extractor_max_facts() -> usize {
    5
}

fn default_hash_dims() -> usize {
    32
}

fn default_memory_budget_fraction() -> f32 {
    0.1
}

fn default_openai_model() -> String {
    "text-embedding-3-small".to_string()
}

fn default_recall_k() -> usize {
    5
}

fn default_sqlite_path() -> PathBuf {
    PathBuf::from("./coulisse-memory.db")
}

fn default_voyage_model() -> String {
    "voyage-3.5".to_string()
}
