use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::TokenCount;

/// User-facing YAML shape for the `memory:` block. Two pillars:
///
/// - `storage` — where the `SQLite` database lives (path, or `:memory:` for
///   ephemeral).
/// - `user_state` — long-term, per-user facts and preferences. Off by default;
///   `user_state: true` enables it with auto-derived defaults; an explicit
///   struct lets advanced users override the embedder, the extraction model,
///   and recall/dedup tuning.
///
/// Resolved into a [`MemoryConfig`] by the orchestrator (cli) before being
/// handed to [`Store::open`]. Resolution looks at the configured `providers:`
/// map to fill in any auto-derived choices.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryYaml {
    #[serde(default)]
    pub storage: Option<String>,
    #[serde(default)]
    pub user_state: UserStateYaml,
}

/// Three forms accepted in YAML for `user_state`:
///
/// - `user_state: false` (or absent) — long-term memory is off entirely. No
///   extraction, no recall, no embedder calls.
/// - `user_state: true` — enabled with auto-derived defaults.
/// - `user_state: { learn_from: ..., embed_with: ..., ... }` — enabled with
///   explicit overrides.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(untagged)]
pub enum UserStateYaml {
    Configured(UserStateConfig),
    OnOff(bool),
}

impl Default for UserStateYaml {
    fn default() -> Self {
        Self::OnOff(false)
    }
}

impl UserStateYaml {
    /// Returns the configured overrides (if any) and whether long-term
    /// memory is enabled at all. `(enabled, overrides)`.
    #[must_use]
    pub fn parts(&self) -> (bool, Option<&UserStateConfig>) {
        match self {
            Self::Configured(cfg) => (true, Some(cfg)),
            Self::OnOff(false) => (false, None),
            Self::OnOff(true) => (true, None),
        }
    }
}

/// Explicit overrides for long-term user state. Every field is optional —
/// resolution fills in the rest from sensible defaults.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserStateConfig {
    /// Cosine similarity threshold above which an extracted fact is considered
    /// a duplicate of an existing memory. Defaults to 0.9. Advanced.
    #[serde(default)]
    pub dedup_threshold: Option<f32>,
    /// Embedder used to vectorize facts for recall and dedup. Auto-picked from
    /// the configured providers when unset.
    #[serde(default)]
    pub embed_with: Option<EmbedderYaml>,
    /// Model that decides what's worth remembering after each exchange.
    /// Auto-picked from the configured providers when unset.
    #[serde(default)]
    pub learn_from: Option<ProviderModel>,
    /// Cap on facts written per exchange. Defaults to 5. Advanced.
    #[serde(default)]
    pub max_facts_per_turn: Option<usize>,
    /// Number of memories recalled per request. Defaults to 5. Advanced.
    #[serde(default)]
    pub recall_k: Option<usize>,
}

/// Generic `{provider, model}` pair used by overrides.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModel {
    pub model: String,
    pub provider: String,
}

/// Embedder override. Mirrors [`EmbedderConfig`] but lives on the YAML side
/// so the resolved type can stay free of optional fields.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(tag = "provider", rename_all = "lowercase", deny_unknown_fields)]
pub enum EmbedderYaml {
    Hash {
        #[serde(default)]
        dims: Option<usize>,
    },
    Openai {
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    Voyage {
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
}

/// Resolved, runtime memory config. Built from a [`MemoryYaml`] plus
/// knowledge of the configured providers. This is the shape that
/// [`Store::open`](crate::Store::open) consumes.
#[derive(Clone, Debug)]
pub struct MemoryConfig {
    pub backend: BackendConfig,
    pub context_budget: TokenCount,
    pub embedder: EmbedderConfig,
    pub extractor: Option<ExtractorConfig>,
    pub memory_budget_fraction: f32,
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

/// Where memory data is stored. `InMemory` is an ephemeral `SQLite` database
/// that evaporates with the process — useful for tests and short-lived demos.
/// `Sqlite` is a file-backed database; for Docker deployments, point `path`
/// at a volume-mounted directory so data survives container restarts.
#[derive(Clone, Debug)]
pub enum BackendConfig {
    InMemory,
    Sqlite { path: PathBuf },
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self::Sqlite {
            path: default_sqlite_path(),
        }
    }
}

impl BackendConfig {
    /// Parse the YAML `storage:` value. `:memory:` (or empty/absent → handled
    /// upstream) maps to ephemeral `SQLite`; anything else is a filesystem path.
    #[must_use]
    pub fn from_storage(value: &str) -> Self {
        if value == ":memory:" {
            Self::InMemory
        } else {
            Self::Sqlite {
                path: PathBuf::from(value),
            }
        }
    }
}

/// Which embedder turns text into vectors. The `hash` provider is a
/// deterministic bag-of-words embedder suitable only for tests and
/// air-gapped development — it has no semantic understanding.
#[derive(Clone, Debug)]
pub enum EmbedderConfig {
    Hash {
        dims: usize,
    },
    Openai {
        api_key: Option<String>,
        model: String,
    },
    Voyage {
        api_key: Option<String>,
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
    #[must_use]
    pub fn model_id(&self) -> String {
        match self {
            Self::Hash { dims } => format!("hash-{dims}"),
            Self::Openai { model, .. } => format!("openai:{model}"),
            Self::Voyage { model, .. } => format!("voyage:{model}"),
        }
    }
}

/// Resolved configuration for the auto-extractor that mines durable facts
/// from each exchange. Constructed by cli from [`MemoryYaml`] + the
/// providers map.
#[derive(Clone, Debug)]
pub struct ExtractorConfig {
    pub dedup_threshold: f32,
    pub max_facts_per_turn: usize,
    pub model: String,
    pub provider: String,
}

#[must_use]
pub fn default_dedup_threshold() -> f32 {
    0.9
}

#[must_use]
pub fn default_extractor_max_facts() -> usize {
    5
}

#[must_use]
pub fn default_hash_dims() -> usize {
    32
}

#[must_use]
pub fn default_recall_k() -> usize {
    5
}

/// Bare filename of the default `SQLite` database. The orchestrator (cli)
/// places it under the project's `.coulisse/` state dir; standalone use of
/// the crate falls back to the current directory via [`default_sqlite_path`].
pub const DEFAULT_SQLITE_FILENAME: &str = "coulisse-memory.db";

#[must_use]
pub fn default_sqlite_path() -> PathBuf {
    PathBuf::from(format!("./{DEFAULT_SQLITE_FILENAME}"))
}

#[must_use]
pub fn default_voyage_model() -> String {
    "voyage-3.5".to_string()
}

#[must_use]
pub fn default_openai_embedding_model() -> String {
    "text-embedding-3-small".to_string()
}

fn default_context_budget() -> TokenCount {
    TokenCount(8_000)
}

fn default_memory_budget_fraction() -> f32 {
    0.1
}
