//! CodeSearch tool: hybrid BM25 + vector semantic search.
//!
//! Two modes:
//! 1. BM25 only (default): tantivy full-text search. No API calls.
//! 2. BM25 + Vector: BM25 candidates merged with HNSW vector k-NN search
//!    backed by the `cersei-embeddings` crate (Gemini / OpenAI / custom).

use crate::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use async_trait::async_trait;
use cersei_embeddings::{EmbeddingProvider, Metric, VectorIndex};
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexReader, ReloadPolicy, TantivyDocument};

// ─── Config ────────────────────────────────────────────────────────────────

const CHUNK_LINES: usize = 50;
const CHUNK_OVERLAP: usize = 10;
const BM25_CANDIDATES: usize = 20;
const VECTOR_CANDIDATES: usize = 20;
const DEFAULT_RESULTS: usize = 10;
const CHUNK_EMBED_CHARS: usize = 500;

const INDEXED_EXTENSIONS: &[&str] = &[
    "bash", "c", "cc", "cpp", "cs", "css", "go", "h", "hh", "hpp",
    "htm", "html", "java", "js", "json", "jsx", "kt", "lua", "md",
    "mjs", "proto", "py", "rb", "rs", "sass", "scss", "sh", "sql",
    "swift", "toml", "ts", "tsx", "txt", "xml", "yaml", "yml", "zsh",
    "cjs", "graphql", "gql", "jsonc", "ml", "mli", "f90", "f95",
    "cobol", "cbl", "ocaml",
];

// ─── Chunk metadata ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ChunkMeta {
    path: String,
    start_line: usize,
    end_line: usize,
    content: String,
}

// ─── Cached index ──────────────────────────────────────────────────────────

struct CachedIndex {
    working_dir: PathBuf,
    // BM25
    bm25_index: Index,
    reader: IndexReader,
    path_field: Field,
    content_field: Field,
    lines_field: Field,
    // Vector (optional)
    vector_index: Option<VectorIndex>,
    chunks: Vec<ChunkMeta>, // chunk_id (vector key) → metadata
}

static INDEX_CACHE: Lazy<Mutex<Option<CachedIndex>>> = Lazy::new(|| Mutex::new(None));

// ─── Indexing ──────────────────────────────────────────────────────────────

fn should_index(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| INDEXED_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn chunk_file(path: &Path, content: &str) -> Vec<ChunkMeta> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return vec![];
    }
    let path_str = path.display().to_string();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let end = (start + CHUNK_LINES).min(lines.len());
        let chunk_content = lines[start..end].join("\n");
        if !chunk_content.trim().is_empty() {
            chunks.push(ChunkMeta {
                path: path_str.clone(),
                content: chunk_content,
                start_line: start + 1,
                end_line: end,
            });
        }
        if end >= lines.len() { break; }
        start += CHUNK_LINES - CHUNK_OVERLAP;
    }
    chunks
}

fn build_bm25_index(chunks: &[ChunkMeta]) -> Result<(Index, IndexReader, Field, Field, Field), String> {
    let mut schema_builder = Schema::builder();
    let path_field = schema_builder.add_text_field("path", STRING | STORED);
    let content_field = schema_builder.add_text_field("content", TEXT | STORED);
    let lines_field = schema_builder.add_text_field("lines", STRING | STORED);
    let schema = schema_builder.build();

    let index = Index::create_in_ram(schema);
    let mut writer = index.writer(50_000_000).map_err(|e| format!("Writer error: {e}"))?;

    for chunk in chunks {
        writer.add_document(doc!(
            path_field => chunk.path.clone(),
            content_field => chunk.content.clone(),
            lines_field => format!("{}:{}", chunk.start_line, chunk.end_line),
        )).map_err(|e| format!("Add doc error: {e}"))?;
    }

    writer.commit().map_err(|e| format!("Commit error: {e}"))?;

    let reader = index.reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .map_err(|e| format!("Reader error: {e}"))?;

    Ok((index, reader, path_field, content_field, lines_field))
}

fn collect_chunks(working_dir: &Path) -> Vec<ChunkMeta> {
    let mut all_chunks = Vec::new();
    for entry in walkdir::WalkDir::new(working_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            !name.starts_with('.') && name != "node_modules" && name != "target"
                && name != "__pycache__" && name != ".venv" && name != "venv"
        })
    {
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        if !entry.file_type().is_file() || !should_index(entry.path()) { continue; }
        if let Ok(meta) = entry.path().metadata() {
            if meta.len() > 500_000 { continue; }
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            all_chunks.extend(chunk_file(entry.path(), &content));
        }
    }
    all_chunks
}

fn build_index(working_dir: &Path, embeddings: Option<Vec<Vec<f32>>>) -> Result<CachedIndex, String> {
    let chunks = collect_chunks(working_dir);
    let file_count = chunks.iter().map(|c| &c.path).collect::<std::collections::HashSet<_>>().len();
    tracing::info!("CodeSearch: indexed {file_count} files, {} chunks", chunks.len());

    let (bm25_index, reader, path_field, content_field, lines_field) = build_bm25_index(&chunks)?;

    let vector_index = if let Some(embs) = embeddings {
        if !embs.is_empty() && !embs[0].is_empty() {
            match VectorIndex::from_vectors(&embs, Metric::Cosine) {
                Ok(idx) => Some(idx),
                Err(e) => {
                    tracing::warn!("Vector index failed, BM25 only: {e}");
                    None
                }
            }
        } else { None }
    } else { None };

    Ok(CachedIndex {
        working_dir: working_dir.to_path_buf(),
        bm25_index,
        reader,
        path_field,
        content_field,
        lines_field,
        vector_index,
        chunks,
    })
}

// ─── Search ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SearchResult {
    path: String,
    content: String,
    start_line: usize,
    end_line: usize,
    bm25_score: f32,
    vector_score: f32,
    final_score: f32,
}

fn bm25_search(cached: &CachedIndex, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
    let searcher = cached.reader.searcher();
    let qp = QueryParser::for_index(&cached.bm25_index, vec![cached.content_field]);
    let parsed = qp.parse_query(query).map_err(|e| format!("Query parse: {e}"))?;
    let top = searcher.search(&parsed, &TopDocs::with_limit(limit)).map_err(|e| format!("Search: {e}"))?;

    let mut results = Vec::new();
    for (score, addr) in top {
        let doc: TantivyDocument = searcher.doc(addr).map_err(|e| format!("Doc: {e}"))?;
        let path = doc.get_first(cached.path_field).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let content = doc.get_first(cached.content_field).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let lines = doc.get_first(cached.lines_field).and_then(|v| v.as_str()).unwrap_or("0:0").to_string();
        let (start, end) = lines.split_once(':')
            .map(|(s, e)| (s.parse().unwrap_or(0), e.parse().unwrap_or(0)))
            .unwrap_or((0, 0));
        results.push(SearchResult {
            path, content, start_line: start, end_line: end,
            bm25_score: score, vector_score: 0.0, final_score: score,
        });
    }
    Ok(results)
}

fn vector_search(cached: &CachedIndex, query_embedding: &[f32], limit: usize) -> Result<Vec<SearchResult>, String> {
    let vi = cached.vector_index.as_ref().ok_or("No vector index")?;
    let hits = vi.search(query_embedding, limit).map_err(|e| format!("Vector search: {e}"))?;

    let mut results = Vec::new();
    for hit in hits {
        let key = hit.key as usize;
        if key < cached.chunks.len() {
            let chunk = &cached.chunks[key];
            results.push(SearchResult {
                path: chunk.path.clone(),
                content: chunk.content.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                bm25_score: 0.0,
                vector_score: hit.similarity,
                final_score: hit.similarity * 100.0,
            });
        }
    }
    Ok(results)
}

fn merge_results(bm25: Vec<SearchResult>, vector: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
    let mut merged: HashMap<String, SearchResult> = HashMap::new();

    // Normalize BM25 scores to 0-1 range
    let max_bm25 = bm25.iter().map(|r| r.bm25_score).fold(0.0f32, f32::max).max(1.0);

    for mut r in bm25 {
        let key = format!("{}:{}:{}", r.path, r.start_line, r.end_line);
        r.bm25_score /= max_bm25; // normalize to 0-1
        merged.insert(key, r);
    }

    for r in vector {
        let key = format!("{}:{}:{}", r.path, r.start_line, r.end_line);
        if let Some(existing) = merged.get_mut(&key) {
            existing.vector_score = r.vector_score;
        } else {
            merged.insert(key, r);
        }
    }

    // Blend: 60% BM25 + 40% vector
    let mut results: Vec<SearchResult> = merged.into_values().map(|mut r| {
        r.final_score = r.bm25_score * 0.6 + r.vector_score * 0.4;
        r
    }).collect();

    results.sort_by(|a, b| b.final_score.partial_cmp(&a.final_score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    results
}

// ─── Tool implementation ───────────────────────────────────────────────────

pub struct CodeSearchTool {
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
}

impl CodeSearchTool {
    /// BM25-only search. No network, no API key required.
    pub fn new() -> Self {
        Self { embedding_provider: None }
    }

    /// Enable hybrid BM25 + vector search using the given embedding provider.
    ///
    /// Use [`cersei_embeddings::auto_from_model`] to construct a provider
    /// from an LLM model string, or build one explicitly with
    /// [`cersei_embeddings::GeminiEmbeddings`] / [`cersei_embeddings::OpenAiEmbeddings`].
    pub fn with_embeddings(provider: Arc<dyn EmbeddingProvider>) -> Self {
        Self { embedding_provider: Some(provider) }
    }
}

impl Default for CodeSearchTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl Tool for CodeSearchTool {
    fn name(&self) -> &str { "CodeSearch" }

    fn description(&self) -> &str {
        "Semantic code search across the codebase. Use natural language queries about behavior, \
         patterns, or concepts. Returns relevant code snippets with file paths and line numbers. \
         This is your DEFAULT tool for discovering code — use it before Grep when you need to \
         understand how something works rather than find an exact string."
    }

    fn permission_level(&self) -> PermissionLevel { PermissionLevel::ReadOnly }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language search query about code behavior, patterns, or concepts."
                },
                "path": { "type": "string", "description": "Directory to search in." },
                "limit": { "type": "integer", "description": "Max results (default: 10)." }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input { query: String, path: Option<String>, limit: Option<usize> }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };

        let search_dir = input.path.map(PathBuf::from).unwrap_or_else(|| ctx.working_dir.clone());
        let limit = input.limit.unwrap_or(DEFAULT_RESULTS);

        // Build or retrieve index
        let needs_build = {
            let cache = INDEX_CACHE.lock().unwrap();
            cache.as_ref().map(|c| c.working_dir != search_dir).unwrap_or(true)
        };

        if needs_build {
            // Collect chunks first
            let chunks = collect_chunks(&search_dir);
            let chunk_texts: Vec<String> = chunks
                .iter()
                .map(|c| c.content.chars().take(CHUNK_EMBED_CHARS).collect())
                .collect();

            // Optionally embed all chunks via the configured provider
            let embeddings = if let Some(provider) = &self.embedding_provider {
                if chunk_texts.is_empty() {
                    None
                } else {
                    match provider.embed_batch(&chunk_texts).await {
                        Ok(embs) => Some(embs),
                        Err(e) => {
                            tracing::warn!("Embedding failed, BM25 only: {e}");
                            None
                        }
                    }
                }
            } else { None };

            match build_index(&search_dir, embeddings) {
                Ok(idx) => { *INDEX_CACHE.lock().unwrap() = Some(idx); }
                Err(e) => return ToolResult::error(format!("Index error: {e}")),
            }
        }

        // Search BM25 and vector (release lock before any await)
        let (bm25_results, has_vector) = {
            let cache = INDEX_CACHE.lock().unwrap();
            let cached = match cache.as_ref() {
                Some(c) => c,
                None => return ToolResult::error("No index available"),
            };
            let bm25 = match bm25_search(cached, &input.query, BM25_CANDIDATES) {
                Ok(r) => r,
                Err(e) => return ToolResult::error(format!("BM25 error: {e}")),
            };
            (bm25, cached.vector_index.is_some())
        }; // lock released here

        // Vector search needs async embedding call, then re-acquires lock briefly
        let results = if has_vector {
            if let Some(provider) = &self.embedding_provider {
                match provider.embed(&input.query).await {
                    Ok(query_emb) => {
                        let cache = INDEX_CACHE.lock().unwrap();
                        let cached = cache.as_ref().unwrap();
                        let vec_results = vector_search(cached, &query_emb, VECTOR_CANDIDATES).unwrap_or_default();
                        drop(cache);
                        merge_results(bm25_results, vec_results, limit)
                    }
                    Err(e) => {
                        tracing::warn!("Query embedding failed, BM25 only: {e}");
                        let mut r = bm25_results; r.truncate(limit); r
                    }
                }
            } else {
                let mut r = bm25_results; r.truncate(limit); r
            }
        } else {
            let mut r = bm25_results; r.truncate(limit); r
        };

        if results.is_empty() {
            return ToolResult::success("No results found. Try different search terms or use Grep for exact patterns.");
        }

        let mut output = String::new();
        for (i, r) in results.iter().enumerate() {
            output.push_str(&format!(
                "── Result {} ── {}:{}-{} (score: {:.2})\n{}\n\n",
                i + 1, r.path, r.start_line, r.end_line, r.final_score, r.content
            ));
        }
        ToolResult::success(output)
    }
}
