use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rig::embeddings::EmbeddingModel as RigEmbeddingModel;
use rig::providers::{openai, voyageai};

use crate::{ConfigError, EmbedError, EmbedderConfig};

/// Runtime-dispatched embedder. One variant per supported provider. The
/// enum is preferred over `dyn Trait` so every embedder is a concrete,
/// monomorphized type and no `async_trait` shim is needed.
pub enum BundledEmbedder {
    Hash(HashEmbedder),
    Openai {
        dims: usize,
        model: openai::EmbeddingModel<reqwest::Client>,
    },
    Voyage {
        dims: usize,
        model: voyageai::EmbeddingModel<reqwest::Client>,
    },
}

impl BundledEmbedder {
    /// Build an embedder from config. `fallback_api_key` is used when the
    /// embedder config doesn't carry its own key — typically the matching
    /// entry from the top-level `providers:` map (so users who already
    /// configured `OpenAI` for completions don't need to repeat the key).
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub fn from_config(
        config: &EmbedderConfig,
        fallback_api_key: Option<&str>,
    ) -> Result<Self, ConfigError> {
        match config {
            EmbedderConfig::Hash { dims } => Ok(Self::Hash(HashEmbedder::new(*dims))),
            EmbedderConfig::Openai { api_key, model } => {
                let key = api_key.as_deref().or(fallback_api_key).ok_or_else(|| {
                    ConfigError::MissingApiKey {
                        provider: "openai".into(),
                    }
                })?;
                let client =
                    openai::Client::new(key).map_err(|source| ConfigError::ClientInit {
                        provider: "openai".into(),
                        message: source.to_string(),
                    })?;
                let dims = openai_dims(model)?;
                Ok(Self::Openai {
                    model: openai::EmbeddingModel::new(client, model, dims),
                    dims,
                })
            }
            EmbedderConfig::Voyage { api_key, model } => {
                let key = api_key.as_deref().or(fallback_api_key).ok_or_else(|| {
                    ConfigError::MissingApiKey {
                        provider: "voyage".into(),
                    }
                })?;
                let client =
                    voyageai::Client::new(key).map_err(|source| ConfigError::ClientInit {
                        provider: "voyage".into(),
                        message: source.to_string(),
                    })?;
                let dims = voyage_dims(model)?;
                Ok(Self::Voyage {
                    model: voyageai::EmbeddingModel::new(client, model, dims),
                    dims,
                })
            }
        }
    }

    #[must_use]
    pub fn ndims(&self) -> usize {
        match self {
            Self::Hash(h) => h.ndims(),
            Self::Openai { dims, .. } | Self::Voyage { dims, .. } => *dims,
        }
    }

    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        match self {
            Self::Hash(h) => Ok(h.embed(text)),
            Self::Openai { model, .. } => {
                let embedding = model.embed_text(text).await.map_err(EmbedError::from)?;
                Ok(to_f32(embedding.vec))
            }
            Self::Voyage { model, .. } => {
                let embedding = model.embed_text(text).await.map_err(EmbedError::from)?;
                Ok(to_f32(embedding.vec))
            }
        }
    }
}

/// Deterministic bag-of-words hashing embedder. Shared vocabulary yields
/// positive cosine similarity so tests and offline demos behave sensibly,
/// but it has no semantic understanding — never use it in production.
pub struct HashEmbedder {
    dims: usize,
}

impl HashEmbedder {
    #[must_use]
    /// # Panics
    ///
    /// Panics if invariants documented above are violated.
    pub fn new(dims: usize) -> Self {
        assert!(dims > 0, "dims must be positive");
        Self { dims }
    }

    #[must_use]
    pub fn ndims(&self) -> usize {
        self.dims
    }

    #[must_use]
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dims];
        for word in text.to_lowercase().split_whitespace() {
            let mut hasher = DefaultHasher::new();
            word.hash(&mut hasher);
            // Hash output is u64; usize is at least 32 bits everywhere we
            // build, so truncating with `as` matches usize::MAX on the host.
            #[allow(clippy::cast_possible_truncation)]
            let idx = (hasher.finish() as usize) % self.dims;
            v[idx] += 1.0;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

fn to_f32(vec: Vec<f64>) -> Vec<f32> {
    // Embeddings are bounded magnitudes; the f64 → f32 narrowing is
    // intrinsic to storing them packed in SQLite.
    #[allow(clippy::cast_possible_truncation)]
    vec.into_iter().map(|x| x as f32).collect()
}

fn openai_dims(model: &str) -> Result<usize, ConfigError> {
    match model {
        "text-embedding-3-large" => Ok(3_072),
        "text-embedding-3-small" | "text-embedding-ada-002" => Ok(1_536),
        other => Err(ConfigError::UnknownModel {
            model: other.into(),
            provider: "openai".into(),
        }),
    }
}

fn voyage_dims(model: &str) -> Result<usize, ConfigError> {
    match model {
        "voyage-3-large" | "voyage-3.5" | "voyage-3.5-lite" | "voyage-code-3"
        | "voyage-finance-2" | "voyage-law-2" => Ok(1_024),
        "voyage-code-2" => Ok(1_536),
        other => Err(ConfigError::UnknownModel {
            model: other.into(),
            provider: "voyage".into(),
        }),
    }
}
