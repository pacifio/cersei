use async_trait::async_trait;

use crate::EmbeddingError;

/// Produces vector embeddings from text.
///
/// Implement this trait to plug in a new embedding backend (Voyage, Cohere,
/// a local model, etc.). The built-in [`GeminiEmbeddings`](crate::GeminiEmbeddings)
/// and [`OpenAiEmbeddings`](crate::OpenAiEmbeddings) types cover the common
/// hosted APIs.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// A short identifier — e.g. `"openai"`, `"gemini"` — useful for logging.
    fn name(&self) -> &str;

    /// Dimensionality of the vectors this provider emits. Used by
    /// [`VectorIndex`](crate::VectorIndex) to size the HNSW graph.
    fn dimensions(&self) -> usize;

    /// Embed a single string. Default implementation delegates to
    /// [`embed_batch`](Self::embed_batch).
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let mut out = self.embed_batch(&[text.to_string()]).await?;
        out.pop()
            .ok_or_else(|| EmbeddingError::Api("empty response".into()))
    }

    /// Embed a batch of strings. Implementations should handle provider-specific
    /// batch-size limits internally (e.g., Gemini caps batches at 100).
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError>;
}
