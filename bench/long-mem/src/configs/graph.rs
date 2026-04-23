//! Config C — GraphMemory (grafeo) substring recall.
//!
//! Each turn becomes a `:Memory` node. Sessions and topics are linked. At
//! retrieval time we use `GraphMemory::recall_top_k` which pulls substring
//! candidates and re-ranks by query-word-overlap.
//!
//! This is the "honest floor" for Cersei's current graph stack — no LLM fact
//! extraction, no semantic matching. If embed beats this on `single-session-*`,
//! that confirms semantic retrieval matters. If hybrid (Config D) beats embed
//! on `multi-session` + `knowledge-update`, that confirms the graph layer is
//! pulling its weight.

use crate::configs::{Config, DEFAULT_TOP_K};
use crate::dataset::Question;
use anyhow::Result;
use async_trait::async_trait;
use cersei_memory::graph::GraphMemory;
use cersei_memory::memdir::MemoryType;

pub struct GraphConfig {
    mem: Option<GraphMemory>,
    top_k: usize,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphConfig {
    pub fn new() -> Self {
        Self {
            mem: None,
            top_k: DEFAULT_TOP_K,
        }
    }

    pub fn with_top_k(mut self, k: usize) -> Self {
        self.top_k = k;
        self
    }
}

#[async_trait]
impl Config for GraphConfig {
    fn name(&self) -> &'static str {
        "graph-substring"
    }

    async fn ingest(&mut self, q: &Question) -> Result<()> {
        // Fresh in-memory graph per question — no cross-question bleed.
        let mem =
            GraphMemory::open_in_memory().map_err(|e| anyhow::anyhow!("open_in_memory: {e}"))?;

        for (i, session) in q.haystack_sessions.iter().enumerate() {
            let date = q.haystack_dates.get(i).map(String::as_str).unwrap_or("");
            for turn in session {
                let content = format!("[{date}] {}: {}", turn.role, turn.content);
                // Low confidence for assistant replies, higher for user turns
                // (factoids usually come from the user telling the assistant
                // something). This nudges recall scoring slightly.
                let conf = if turn.role == "user" { 0.9 } else { 0.6 };
                let _ = mem.store_memory(&content, MemoryType::Project, conf);
            }
        }

        self.mem = Some(mem);
        Ok(())
    }

    async fn retrieve(&self, q: &Question) -> Result<String> {
        let Some(mem) = &self.mem else {
            return Ok(String::new());
        };
        let hits = mem.recall_top_k(&q.question, self.top_k);
        let mut out = String::new();
        out.push_str(&format!(
            "Top {} relevant turns (graph substring):\n",
            hits.len()
        ));
        for (i, (content, score)) in hits.iter().enumerate() {
            out.push_str(&format!("{}. (score={:.3}) {}\n", i + 1, score, content));
        }
        Ok(out)
    }
}
