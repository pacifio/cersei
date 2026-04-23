//! `EmbeddingMemory` — bridges `cersei-embeddings` into the `Memory` trait
//! so an `Agent` can consume embedding-backed recall via the same interface
//! as `JsonlMemory` or `InMemory`.
//!
//! Only compiled when the `embed` feature is enabled.

use async_trait::async_trait;
use cersei_embeddings::{EmbeddingProvider, EmbeddingStore, Metric};
use cersei_types::*;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Memory;

/// Memory backend backed by a vector index. Each call to `store` embeds
/// every non-empty text block in the message list and inserts it. `search`
/// runs a k-NN query and returns hits as ranked `MemoryEntry`s.
///
/// This is deliberately simple — it's designed to power evaluations like
/// LongMemEval and as a starting point for RAG-style applications. For more
/// control, drop down to `EmbeddingStore` directly.
pub struct EmbeddingMemory<P: EmbeddingProvider> {
    store: EmbeddingStore<P>,
    // key → (content, session_id) so `search` can return the text and source
    payloads: RwLock<HashMap<u64, (String, String)>>,
    next_key: AtomicU64,
}

impl<P: EmbeddingProvider> EmbeddingMemory<P> {
    pub fn new(provider: P, metric: Metric) -> Result<Self> {
        let store = EmbeddingStore::new(provider, metric)
            .map_err(|e| CerseiError::Config(format!("EmbeddingStore init failed: {e}")))?;
        Ok(Self {
            store,
            payloads: RwLock::new(HashMap::new()),
            next_key: AtomicU64::new(1),
        })
    }

    /// Number of stored vectors.
    pub fn len(&self) -> usize {
        self.payloads.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Add a single text snippet with an explicit source label (e.g. a
    /// session id or fact-extractor tag). Returns the assigned key.
    pub async fn add(&self, text: impl Into<String>, source: impl Into<String>) -> Result<u64> {
        let text = text.into();
        let source = source.into();
        if text.trim().is_empty() {
            return Ok(0);
        }
        let key = self.next_key.fetch_add(1, Ordering::Relaxed);
        self.store
            .add_batch(&[(key, text.clone())])
            .await
            .map_err(|e| CerseiError::Config(format!("embed add: {e}")))?;
        self.payloads.write().insert(key, (text, source));
        Ok(key)
    }

    /// Batch version of [`add`] — embeds everything in one provider call.
    pub async fn add_batch(&self, items: &[(String, String)]) -> Result<Vec<u64>> {
        let items: Vec<(u64, String, String)> = items
            .iter()
            .filter(|(t, _)| !t.trim().is_empty())
            .map(|(t, s)| {
                let key = self.next_key.fetch_add(1, Ordering::Relaxed);
                (key, t.clone(), s.clone())
            })
            .collect();
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let to_store: Vec<(u64, String)> = items.iter().map(|(k, t, _)| (*k, t.clone())).collect();
        self.store
            .add_batch(&to_store)
            .await
            .map_err(|e| CerseiError::Config(format!("embed add_batch: {e}")))?;
        let mut p = self.payloads.write();
        let mut keys = Vec::with_capacity(items.len());
        for (k, t, s) in items {
            p.insert(k, (t, s));
            keys.push(k);
        }
        Ok(keys)
    }
}

#[async_trait]
impl<P: EmbeddingProvider> Memory for EmbeddingMemory<P> {
    async fn store(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        let items: Vec<(String, String)> = messages
            .iter()
            .filter_map(|m| {
                m.get_text()
                    .map(|t| (t.to_string(), session_id.to_string()))
            })
            .collect();
        if !items.is_empty() {
            self.add_batch(&items).await?;
        }
        Ok(())
    }

    async fn load(&self, _session_id: &str) -> Result<Vec<Message>> {
        // Vector memory has no ordering semantics. Use `search` for retrieval.
        Ok(Vec::new())
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        if limit == 0 || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let hits = self
            .store
            .search(query, limit)
            .await
            .map_err(|e| CerseiError::Config(format!("embed search: {e}")))?;
        let payloads = self.payloads.read();
        Ok(hits
            .into_iter()
            .filter_map(|h| {
                payloads.get(&h.key).map(|(text, source)| MemoryEntry {
                    content: text.clone(),
                    relevance: h.similarity,
                    source: source.clone(),
                })
            })
            .collect())
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        // Not tracked — this backend is for retrieval, not session listing.
        Ok(Vec::new())
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        // Soft delete: drop payloads for this session. The underlying
        // usearch index keeps the vectors but they become unreachable via
        // `search` because payload lookup fails.
        self.payloads.write().retain(|_, (_, s)| s != session_id);
        Ok(())
    }
}
