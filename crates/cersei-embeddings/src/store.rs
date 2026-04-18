use crate::{EmbeddingError, EmbeddingProvider, Metric, SearchHit, VectorIndex};

/// A provider + index bundled together for the common case.
///
/// Use this when you want a simple "add text → search by text" API.
/// For lower-level control (e.g., reusing pre-computed embeddings), drop
/// down to [`EmbeddingProvider`] and [`VectorIndex`] directly.
pub struct EmbeddingStore<P: EmbeddingProvider> {
    provider: P,
    index: VectorIndex,
}

impl<P: EmbeddingProvider> EmbeddingStore<P> {
    /// Build an empty store. The index dimensionality is taken from
    /// `provider.dimensions()`.
    pub fn new(provider: P, metric: Metric) -> Result<Self, EmbeddingError> {
        let dim = provider.dimensions();
        let index = VectorIndex::new(dim, metric)?;
        Ok(Self { provider, index })
    }

    /// Embed `items` and insert them into the index.
    ///
    /// Each item is `(key, text)`. Keys are opaque `u64` identifiers you
    /// choose — typically an incrementing counter or a hash.
    pub async fn add_batch(&self, items: &[(u64, String)]) -> Result<(), EmbeddingError> {
        if items.is_empty() {
            return Ok(());
        }
        self.index.reserve(self.index.len() + items.len())?;
        let texts: Vec<String> = items.iter().map(|(_, t)| t.clone()).collect();
        let vectors = self.provider.embed_batch(&texts).await?;
        for ((key, _), vector) in items.iter().zip(vectors.iter()) {
            self.index.add(*key, vector)?;
        }
        Ok(())
    }

    /// Embed `query` and run a k-NN search.
    pub async fn search(&self, query: &str, k: usize) -> Result<Vec<SearchHit>, EmbeddingError> {
        let vec = self.provider.embed(query).await?;
        self.index.search(&vec, k)
    }

    /// Borrow the underlying provider.
    pub fn provider(&self) -> &P { &self.provider }

    /// Borrow the underlying index (e.g., to inspect `len()`).
    pub fn index(&self) -> &VectorIndex { &self.index }
}
