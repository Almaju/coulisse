//! Deterministic embedder for tests. Not for production use.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::{EmbedError, Embedder};

/// Bag-of-words hashing embedder. Texts sharing vocabulary yield vectors with
/// positive cosine similarity, so recall tests behave sensibly.
pub struct HashEmbedder {
    dims: usize,
}

impl HashEmbedder {
    pub fn new(dims: usize) -> Self {
        assert!(dims > 0, "dims must be positive");
        Self { dims }
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(32)
    }
}

impl Embedder for HashEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let mut v = vec![0.0f32; self.dims];
        for word in text.to_lowercase().split_whitespace() {
            let mut hasher = DefaultHasher::new();
            word.hash(&mut hasher);
            let idx = (hasher.finish() as usize) % self.dims;
            v[idx] += 1.0;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Ok(v)
    }

    fn ndims(&self) -> usize {
        self.dims
    }
}
