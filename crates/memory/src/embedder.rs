use std::future::Future;

use crate::EmbedError;

pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> impl Future<Output = Result<Vec<f32>, EmbedError>> + Send;

    fn ndims(&self) -> usize;
}
