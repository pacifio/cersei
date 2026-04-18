use crate::EmbeddingError;

/// Similarity metric used by [`VectorIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// Cosine distance. `similarity = 1.0 - distance`.
    Cosine,
    /// Squared Euclidean distance. `similarity = 1.0 / (1.0 + distance)`.
    L2,
    /// Inner product — higher is more similar. `similarity = distance`.
    InnerProduct,
}

impl Metric {
    fn to_usearch(self) -> usearch::MetricKind {
        match self {
            Metric::Cosine => usearch::MetricKind::Cos,
            Metric::L2 => usearch::MetricKind::L2sq,
            Metric::InnerProduct => usearch::MetricKind::IP,
        }
    }

    fn similarity_from_distance(self, distance: f32) -> f32 {
        match self {
            Metric::Cosine => 1.0 - distance,
            Metric::L2 => 1.0 / (1.0 + distance),
            Metric::InnerProduct => distance,
        }
    }
}

/// A single match returned by [`VectorIndex::search`].
#[derive(Debug, Clone, Copy)]
pub struct SearchHit {
    /// The key that was passed to [`VectorIndex::add`].
    pub key: u64,
    /// Raw distance as reported by `usearch`.
    pub distance: f32,
    /// Distance converted into a similarity score (higher = more similar),
    /// using the [`Metric`] the index was created with.
    pub similarity: f32,
}

/// A `usearch`-backed HNSW vector index.
pub struct VectorIndex {
    inner: usearch::Index,
    metric: Metric,
    dimensions: usize,
}

impl VectorIndex {
    /// Build an empty index. Call [`reserve`](Self::reserve) before adding
    /// many vectors for best performance.
    pub fn new(dimensions: usize, metric: Metric) -> Result<Self, EmbeddingError> {
        let options = usearch::IndexOptions {
            dimensions,
            metric: metric.to_usearch(),
            quantization: usearch::ScalarKind::F32,
            connectivity: 0,
            expansion_add: 0,
            expansion_search: 0,
            multi: false,
        };
        let inner = usearch::Index::new(&options)
            .map_err(|e| EmbeddingError::Index(format!("usearch init: {e}")))?;
        Ok(Self { inner, metric, dimensions })
    }

    /// Build an index and populate it in one call, sized exactly for the
    /// input. Vector `i` is stored under key `i as u64`.
    pub fn from_vectors(
        vectors: &[Vec<f32>],
        metric: Metric,
    ) -> Result<Self, EmbeddingError> {
        if vectors.is_empty() {
            return Err(EmbeddingError::Index("cannot build index from empty vectors".into()));
        }
        let dim = vectors[0].len();
        let index = Self::new(dim, metric)?;
        index.reserve(vectors.len())?;
        for (i, v) in vectors.iter().enumerate() {
            index.add(i as u64, v)?;
        }
        Ok(index)
    }

    /// Hint the index to pre-allocate capacity for `n` vectors.
    pub fn reserve(&self, n: usize) -> Result<(), EmbeddingError> {
        self.inner
            .reserve(n)
            .map_err(|e| EmbeddingError::Index(format!("usearch reserve: {e}")))
    }

    /// Insert a vector.
    pub fn add(&self, key: u64, vector: &[f32]) -> Result<(), EmbeddingError> {
        self.inner
            .add(key, vector)
            .map_err(|e| EmbeddingError::Index(format!("usearch add: {e}")))
    }

    /// k-NN search, returning up to `k` matches ordered by the index.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<SearchHit>, EmbeddingError> {
        let matches = self
            .inner
            .search(query, k)
            .map_err(|e| EmbeddingError::Index(format!("usearch search: {e}")))?;
        let hits = matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .map(|(&key, &distance)| SearchHit {
                key,
                distance,
                similarity: self.metric.similarity_from_distance(distance),
            })
            .collect();
        Ok(hits)
    }

    /// Number of vectors in the index.
    pub fn len(&self) -> usize { self.inner.size() }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool { self.len() == 0 }

    /// The configured dimensionality.
    pub fn dimensions(&self) -> usize { self.dimensions }

    /// The configured metric.
    pub fn metric(&self) -> Metric { self.metric }
}
