//! Config B — EmbeddingMemory (usearch HNSW cosine) semantic recall.
//!
//! Each turn becomes one vector indexed under `session_id`. At retrieval time
//! we embed the question, pull top-k nearest turns, and join them with a
//! session header so the answerer knows the provenance.

use crate::configs::{Config, DEFAULT_TOP_K};
use crate::dataset::Question;
use anyhow::Result;
use async_trait::async_trait;
use cersei_embeddings::{EmbeddingProvider, Metric};
use cersei_memory::embedding_memory::EmbeddingMemory;
use cersei_memory::Memory;

pub struct EmbedConfig<P: EmbeddingProvider + Send + Sync> {
    mem: Option<EmbeddingMemory<P>>,
    provider_factory: Box<dyn Fn() -> P + Send + Sync>,
    top_k: usize,
}

impl<P: EmbeddingProvider + Send + Sync + 'static> EmbedConfig<P> {
    pub fn new(provider_factory: impl Fn() -> P + Send + Sync + 'static) -> Self {
        Self {
            mem: None,
            provider_factory: Box::new(provider_factory),
            top_k: DEFAULT_TOP_K,
        }
    }

    pub fn with_top_k(mut self, k: usize) -> Self {
        self.top_k = k;
        self
    }
}

#[async_trait]
impl<P: EmbeddingProvider + Send + Sync + 'static> Config for EmbedConfig<P> {
    fn name(&self) -> &'static str {
        "embed-only"
    }

    async fn ingest(&mut self, q: &Question) -> Result<()> {
        // Fresh memory per question — haystacks don't share turns across Qs.
        let provider = (self.provider_factory)();
        let mem = EmbeddingMemory::new(provider, Metric::Cosine)?;

        // Flatten haystack into (text, session_id) pairs.
        let mut items: Vec<(String, String)> = Vec::new();
        for (i, session) in q.haystack_sessions.iter().enumerate() {
            let sid = q
                .haystack_session_ids
                .get(i)
                .map(String::as_str)
                .unwrap_or("");
            let date = q.haystack_dates.get(i).map(String::as_str).unwrap_or("");
            for turn in session {
                items.push((
                    format!("[{date}] {}: {}", turn.role, turn.content),
                    sid.to_string(),
                ));
            }
        }
        mem.add_batch(&items).await?;
        self.mem = Some(mem);
        Ok(())
    }

    async fn retrieve(&self, q: &Question) -> Result<String> {
        let Some(mem) = &self.mem else {
            return Ok(String::new());
        };
        let hits = mem.search(&q.question, self.top_k).await?;
        let mut out = String::new();
        out.push_str(&format!(
            "Top {} relevant turns (cosine similarity):\n",
            hits.len()
        ));
        for (i, hit) in hits.iter().enumerate() {
            out.push_str(&format!(
                "{}. (session={} sim={:.3}) {}\n",
                i + 1,
                hit.source,
                hit.relevance,
                hit.content
            ));
        }
        Ok(out)
    }
}
