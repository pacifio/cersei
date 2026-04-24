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

/// Weight discount applied to rank-list contributions that come from
/// expansion variants (as opposed to the primary query). 0.8× matches
/// Omega-memory's `_EXPANSION_WEIGHT_DISCOUNT`.
const EXPANSION_WEIGHT: f32 = 0.8;

/// Omega-memory's empirical abstention floors (v0.1.8). Hits below these
/// thresholds are discarded before RRF fusion — prevents noise-ridden
/// candidates from winning on an otherwise empty haystack. Source:
/// `_inspirations/omega-memory/src/omega/sqlite_store/_types.py:73-80`.
const VEC_MIN_SIM: f32 = 0.35;
const GRAPH_MIN_SCORE: f32 = 0.30;

/// Cosine-similarity threshold for treating two embedded facts as duplicates
/// at ingestion. Matches Omega's default (`SEMANTIC_DEDUP_COSINE = 0.85`).
#[allow(dead_code)]
const DEDUP_COSINE: f32 = 0.85;

/// Jaccard word-overlap threshold for treating two facts as duplicates at
/// ingestion. Text-only proxy for cosine dedup — avoids a 2-pass embed-
/// then-compare. 0.85 is Omega's per-type default for `lesson_learned`.
const DEDUP_JACCARD: f32 = 0.85;

/// Minimum token length of a fact worth keeping. Shorter strings are often
/// empty parse artifacts ("* ", ">", etc.) and pollute retrieval.
const MIN_FACT_TOKENS: usize = 3;

fn normalize_for_dedup(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn word_set(s: &str) -> std::collections::HashSet<&str> {
    s.split_whitespace().filter(|w| w.len() > 2).collect()
}

fn jaccard(a: &str, b: &str) -> f32 {
    let sa = word_set(a);
    let sb = word_set(b);
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    let inter = sa.intersection(&sb).count();
    let uni = sa.union(&sb).count();
    inter as f32 / uni as f32
}

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
    /// Whether to expand the query into lex/vec/HyDE variants at retrieval
    /// time (Omega-memory lever — +1–2 pp lift on multi-session / vague).
    use_query_expansion: bool,
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
            use_query_expansion: true,
            extracted_fact_count: 0,
        }
    }

    pub fn with_top_k(mut self, k: usize) -> Self {
        self.top_k = k;
        self
    }

    pub fn with_query_expansion(mut self, on: bool) -> Self {
        self.use_query_expansion = on;
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
        // Normalized form → canonical tagged string, for exact-match dedup.
        let mut seen_normalized: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        // Parallel vec of normalized strings for Jaccard fuzzy dedup.
        let mut normalized_items: Vec<String> = Vec::new();

        while let Some(joined) = set.join_next().await {
            let (date, sid, session, facts) =
                joined.map_err(|e| anyhow::anyhow!("fact join: {e}"))?;

            // Store BOTH the raw turns AND the extracted facts. The facts
            // give RRF a high-signal summary to rank against; the raw turns
            // rescue us when the extractor drops specifics ("Summer Vibes",
            // "The Glass Menagerie", etc.).
            let raw: Vec<String> = session
                .iter()
                .map(|t| format!("{}: {}", t.role, t.content))
                .collect();

            for item in raw.iter().chain(facts.iter()) {
                let tagged = format!("[{date}] {item}");
                let norm = normalize_for_dedup(&tagged);
                let token_count = norm.split_whitespace().count();
                if token_count < MIN_FACT_TOKENS {
                    continue;
                }

                // (1) Exact-match dedup: identical normalized form already
                //     stored. Skip silently.
                if seen_normalized.contains_key(&norm) {
                    continue;
                }
                // (2) Jaccard near-duplicate. Linear scan is O(n²) but n is
                //     small (≤ a few thousand per question) and tokens are
                //     already set-ified inside `jaccard`. Fine for a bench.
                let near_dup = normalized_items
                    .iter()
                    .any(|existing| jaccard(existing, &norm) >= DEDUP_JACCARD);
                if near_dup {
                    continue;
                }

                seen_normalized.insert(norm.clone(), embed_items.len());
                normalized_items.push(norm);
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
        // Pull a generous candidate pool (2×top_k) so that the abstention
        // filter has room before RRF truncation.
        let pool = self.top_k.saturating_mul(2).max(self.top_k);

        // Optional: expand the question into lex/vec/HyDE variants before
        // retrieval. Runs one LLM call on the extractor model (Omega-memory
        // lever). Disabled on trivially-short queries where expansion adds
        // cost without recall gain.
        let variants = if self.use_query_expansion && q.question.trim().len() >= 3 {
            let include_hyde = crate::query_expand::looks_vague(&q.question);
            crate::query_expand::expand_query(
                &*self.extractor,
                &self.extractor_model,
                &q.question,
                include_hyde,
            )
            .await
            .unwrap_or_default()
        } else {
            crate::query_expand::QueryVariants::default()
        };

        // Fused scores, keyed by canonical content (facts are stored
        // verbatim so same content merges naturally across rank lists).
        let mut fused: HashMap<String, f32> = HashMap::new();

        // Helper: apply Omega's RRF to a ranked list with a channel weight.
        let apply_rrf =
            |fused: &mut HashMap<String, f32>, list: &[String], weight: f32| {
                for (rank, content) in list.iter().enumerate() {
                    let score = weight / (RRF_K + (rank as f32 + 1.0));
                    *fused.entry(content.clone()).or_insert(0.0) += score;
                }
            };

        // --- Primary query (embed + graph) ---
        if let Some(m) = &self.embed_mem {
            let raw = m.search(&q.question, pool).await.unwrap_or_default();
            let list: Vec<String> = raw
                .into_iter()
                .filter(|h| h.relevance >= VEC_MIN_SIM)
                .take(self.top_k)
                .map(|h| h.content)
                .collect();
            apply_rrf(&mut fused, &list, 1.0);
        }
        if let Some(m) = &self.graph_mem {
            let raw = m.recall_top_k(&q.question, pool);
            let list: Vec<String> = raw
                .into_iter()
                .filter(|(_, s)| *s >= GRAPH_MIN_SCORE)
                .take(self.top_k)
                .map(|(c, _)| c)
                .collect();
            apply_rrf(&mut fused, &list, 1.0);
        }

        // --- Semantic variants (vec + HyDE) fuse at EXPANSION_WEIGHT ---
        if let Some(m) = &self.embed_mem {
            for variant in variants.semantic_variants() {
                if variant.trim().is_empty() {
                    continue;
                }
                let raw = m.search(&variant, pool).await.unwrap_or_default();
                let list: Vec<String> = raw
                    .into_iter()
                    .filter(|h| h.relevance >= VEC_MIN_SIM)
                    .take(self.top_k)
                    .map(|h| h.content)
                    .collect();
                apply_rrf(&mut fused, &list, EXPANSION_WEIGHT);
            }
        }

        // --- Lexical variants through graph substring at EXPANSION_WEIGHT ---
        if let Some(m) = &self.graph_mem {
            for lex in &variants.lex {
                if lex.trim().is_empty() {
                    continue;
                }
                let raw = m.recall_top_k(lex, pool);
                let list: Vec<String> = raw
                    .into_iter()
                    .filter(|(_, s)| *s >= GRAPH_MIN_SCORE)
                    .take(self.top_k)
                    .map(|(c, _)| c)
                    .collect();
                apply_rrf(&mut fused, &list, EXPANSION_WEIGHT);
            }
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
