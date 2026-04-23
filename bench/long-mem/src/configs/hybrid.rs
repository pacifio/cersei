//! Config D — Hybrid: LLM fact extraction → store in both EmbeddingMemory
//! and GraphMemory → retrieve via Reciprocal Rank Fusion (RRF).
//!
//! This is the configuration that directly competes with Mastra's
//! "observational memory" research. The pipeline:
//!
//!   1. For each haystack session, call `gpt-4o-mini` once with a
//!      fact-extraction prompt to turn the turns into 3–8 short fact
//!      statements. This is the "observer" pass.
//!   2. Each fact is inserted into both EmbeddingMemory (semantic vector)
//!      and GraphMemory (substring node) tagged with session date + id.
//!   3. At retrieval, run both stores independently for top-k, then fuse
//!      via RRF: score(d) = sum over lists of 1 / (k + rank_in_list).

use crate::configs::{Config, DEFAULT_TOP_K};
use crate::dataset::Question;
use anyhow::{Context, Result};
use async_trait::async_trait;
use cersei_embeddings::{EmbeddingProvider, Metric};
use cersei_memory::embedding_memory::EmbeddingMemory;
use cersei_memory::graph::GraphMemory;
use cersei_memory::memdir::MemoryType;
use cersei_memory::Memory;
use cersei_provider::{CompletionRequest, Provider};
use cersei_types::{Message, Role};
use std::collections::HashMap;

/// RRF constant. 60 is the Elasticsearch/pyserini default; robust across a
/// wide range of rank-list lengths.
const RRF_K: f32 = 60.0;

/// Hard cap on observations stored per session. Mastra's Observer produces
/// 1-5 per exchange; a long session can easily yield 20-30. The cap is a
/// safety net against runaway output, not a pruning hint — set high.
const FACTS_PER_SESSION: usize = 40;

/// Concurrent in-flight fact-extractor calls PER question. The outer runner
/// already fans out across questions, so total concurrency is
/// `outer_concurrency × INNER_EXTRACT_CONCURRENCY` provider calls.
const INNER_EXTRACT_CONCURRENCY: usize = 6;

pub struct HybridConfig<P: EmbeddingProvider + Send + Sync, E: Provider + Send + Sync + ?Sized> {
    embed_mem: Option<EmbeddingMemory<P>>,
    graph_mem: Option<GraphMemory>,
    provider_factory: Box<dyn Fn() -> P + Send + Sync>,
    extractor: std::sync::Arc<E>,
    extractor_model: String,
    top_k: usize,
    /// Cached for reporting.
    extracted_fact_count: usize,
}

impl<
        P: EmbeddingProvider + Send + Sync + 'static,
        E: Provider + Send + Sync + ?Sized + 'static,
    > HybridConfig<P, E>
{
    pub fn new(
        provider_factory: impl Fn() -> P + Send + Sync + 'static,
        extractor: std::sync::Arc<E>,
        extractor_model: impl Into<String>,
    ) -> Self {
        Self {
            embed_mem: None,
            graph_mem: None,
            provider_factory: Box::new(provider_factory),
            extractor,
            extractor_model: extractor_model.into(),
            top_k: DEFAULT_TOP_K,
            extracted_fact_count: 0,
        }
    }

    pub fn with_top_k(mut self, k: usize) -> Self {
        self.top_k = k;
        self
    }

    pub fn fact_count(&self) -> usize {
        self.extracted_fact_count
    }

    /// Ask the Observer LLM to turn a session into a structured observation
    /// list using **Mastra's verbatim Observer prompt**. Returns a list of
    /// observation lines (each is one `🔴/🟡/🟢/✅` bullet, possibly indented).
    /// On any failure we fall back to the raw turn texts so the run never
    /// breaks.
    ///
    /// This is the key 0.1.7 → Mastra-parity change: instead of summarising
    /// into 3–6 bullets with a generic prompt, we run the full Mastra Observer
    /// with its temporal anchoring, assertion-vs-question distinction, and
    /// state-change rules. Those rules directly target LongMemEval's
    /// knowledge-update, temporal-reasoning, and multi-session question types.
    async fn extract_facts(
        extractor: &E,
        model: &str,
        date: &str,
        session: &[crate::dataset::Turn],
    ) -> Result<Vec<String>> {
        if session.is_empty() {
            return Ok(Vec::new());
        }

        // Mastra formats each message with role + timestamp. Since LongMemEval
        // sessions share one date, we only have hour-resolution variability
        // within the session — but we still stamp the date so temporal
        // anchoring can emit "(meaning DATE)" properly.
        let mut history = format!("Date: {date}\n");
        for turn in session {
            // LongMemEval uses "user"/"assistant" verbatim. Mastra's observer
            // expects capitalised role labels like "User"/"Assistant".
            let role_cap = if turn.role == "user" {
                "User"
            } else if turn.role == "assistant" {
                "Assistant"
            } else {
                "System"
            };
            history.push_str(&format!("{role_cap}: {}\n", turn.content));
        }

        let user_prompt = format!(
            "## New Message History to Observe\n\n{history}\n\n---\n\n## Your Task\n\nExtract new observations from the message history above. Output them in the format specified in your instructions."
        );

        let mut req = CompletionRequest::new(model);
        req.system = Some(crate::mastra_prompts::observer_system_prompt());
        req.messages.push(Message::user(user_prompt));
        req.temperature = Some(0.3); // matches Mastra's observation.modelSettings.temperature
        // Generous ceiling — Gemini 2.5 Flash burns thinking tokens before
        // output, and the Observer prompt asks for multi-observation dense
        // output. 8k is what Mastra budgets (`maxOutputTokens: 100_000` in
        // their config, scaled down to our per-session batches).
        req.max_tokens = 8192;

        let resp = extractor
            .complete_blocking(req)
            .await
            .context("observer call failed")?;
        let text = resp.message.get_all_text().to_string();
        let observations_block = crate::mastra_prompts::parse_observations_block(&text);

        // One observation per line, keep lines that start with bullet markers.
        let facts: Vec<String> = observations_block
            .lines()
            .map(str::trim_end)
            .filter(|l| {
                let t = l.trim_start();
                // Keep Date: headers so temporal context survives, plus any
                // line that starts with Mastra's bullet or emoji markers.
                t.starts_with("Date:")
                    || t.starts_with('*')
                    || t.starts_with("🔴")
                    || t.starts_with("🟡")
                    || t.starts_with("🟢")
                    || t.starts_with("✅")
                    || t.starts_with("->")
            })
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .take(FACTS_PER_SESSION)
            .collect();

        Ok(facts)
    }
}

#[async_trait]
impl<
        P: EmbeddingProvider + Send + Sync + 'static,
        E: Provider + Send + Sync + ?Sized + 'static,
    > Config for HybridConfig<P, E>
{
    fn name(&self) -> &'static str {
        "hybrid-embed-graph"
    }

    async fn ingest(&mut self, q: &Question) -> Result<()> {
        let provider = (self.provider_factory)();
        let embed = EmbeddingMemory::new(provider, Metric::Cosine)?;
        let graph =
            GraphMemory::open_in_memory().map_err(|e| anyhow::anyhow!("open_in_memory: {e}"))?;

        // Extract facts from every session concurrently (bounded by a
        // per-question semaphore so we don't hammer the provider). This is
        // the difference between ~30s and ~150s on a 30-session haystack.
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(INNER_EXTRACT_CONCURRENCY));
        let mut set = tokio::task::JoinSet::new();
        for (i, session) in q.haystack_sessions.iter().cloned().enumerate() {
            let date = q
                .haystack_dates
                .get(i)
                .cloned()
                .unwrap_or_else(|| "unknown-date".to_string());
            let sid = q.haystack_session_ids.get(i).cloned().unwrap_or_default();
            let extractor = self.extractor.clone();
            let model = self.extractor_model.clone();
            let permit = sem.clone().acquire_owned().await.unwrap();
            set.spawn(async move {
                let _permit = permit; // released on drop
                let facts = Self::extract_facts(&*extractor, &model, &date, &session)
                    .await
                    .unwrap_or_default();
                (date, sid, session, facts)
            });
        }

        let mut fact_count = 0usize;
        let mut embed_items: Vec<(String, String)> = Vec::new();

        while let Some(joined) = set.join_next().await {
            let (date, sid, session, facts) =
                joined.map_err(|e| anyhow::anyhow!("fact join: {e}"))?;

            // Store BOTH the raw turns AND the extracted facts. The facts
            // give RRF a high-signal summary to rank against; the raw turns
            // rescue us when the extractor drops specifics ("Summer Vibes",
            // "The Glass Menagerie", etc.). Dedup is handled downstream by
            // the RRF key (content string).
            let raw: Vec<String> = session
                .iter()
                .map(|t| format!("{}: {}", t.role, t.content))
                .collect();

            for item in raw.iter().chain(facts.iter()) {
                let tagged = format!("[{date}] {item}");
                embed_items.push((tagged.clone(), sid.clone()));
                let _ = graph.store_memory(&tagged, MemoryType::Project, 0.85);
                fact_count += 1;
            }
        }

        embed.add_batch(&embed_items).await?;

        self.extracted_fact_count = fact_count;
        self.embed_mem = Some(embed);
        self.graph_mem = Some(graph);
        Ok(())
    }

    async fn retrieve(&self, q: &Question) -> Result<String> {
        let embed_hits = match &self.embed_mem {
            Some(m) => m.search(&q.question, self.top_k).await.unwrap_or_default(),
            None => Vec::new(),
        };
        let graph_hits = match &self.graph_mem {
            Some(m) => m.recall_top_k(&q.question, self.top_k),
            None => Vec::new(),
        };

        // Reciprocal Rank Fusion. Key by content string (facts are already
        // stored verbatim, so identical facts across lists merge naturally).
        let mut fused: HashMap<String, f32> = HashMap::new();
        for (rank, hit) in embed_hits.iter().enumerate() {
            let score = 1.0 / (RRF_K + (rank as f32 + 1.0));
            *fused.entry(hit.content.clone()).or_insert(0.0) += score;
        }
        for (rank, (content, _)) in graph_hits.iter().enumerate() {
            let score = 1.0 / (RRF_K + (rank as f32 + 1.0));
            *fused.entry(content.clone()).or_insert(0.0) += score;
        }

        let mut ranked: Vec<(String, f32)> = fused.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(self.top_k);

        let mut out = String::new();
        out.push_str(&format!(
            "Top {} facts (hybrid RRF of embed + graph, from {} extracted):\n",
            ranked.len(),
            self.extracted_fact_count
        ));
        for (i, (content, score)) in ranked.iter().enumerate() {
            out.push_str(&format!("{}. (rrf={:.4}) {}\n", i + 1, score, content));
        }
        Ok(out)
    }
}
