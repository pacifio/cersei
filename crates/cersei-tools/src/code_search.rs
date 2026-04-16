//! CodeSearch tool: hybrid BM25 + vector semantic search.
//!
//! Two modes:
//! 1. BM25 only (default): tantivy full-text search. No API calls.
//! 2. BM25 + Vector (--embedding-api): BM25 candidates merged with USearch
//!    HNSW vector k-NN search using Gemini/OpenAI embeddings.

use crate::{Tool, ToolResult, ToolContext, PermissionLevel, ToolCategory};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexReader, ReloadPolicy, TantivyDocument};
use once_cell::sync::Lazy;

// ─── Config ────────────────────────────────────────────────────────────────

const CHUNK_LINES: usize = 50;
const CHUNK_OVERLAP: usize = 10;
const BM25_CANDIDATES: usize = 20;
const VECTOR_CANDIDATES: usize = 20;
const DEFAULT_RESULTS: usize = 10;

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
    vector_index: Option<usearch::Index>,
    chunks: Vec<ChunkMeta>, // chunk_id (usearch key) → metadata
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

/// Build USearch vector index from pre-computed embeddings.
fn build_vector_index(embeddings: &[Vec<f32>], dim: usize) -> Result<usearch::Index, String> {
    let options = usearch::IndexOptions {
        dimensions: dim,
        metric: usearch::MetricKind::Cos,
        quantization: usearch::ScalarKind::F32,
        connectivity: 0,
        expansion_add: 0,
        expansion_search: 0,
        multi: false,
    };
    let index = usearch::Index::new(&options).map_err(|e| format!("USearch init: {e}"))?;
    index.reserve(embeddings.len()).map_err(|e| format!("USearch reserve: {e}"))?;

    for (i, emb) in embeddings.iter().enumerate() {
        index.add(i as u64, emb).map_err(|e| format!("USearch add: {e}"))?;
    }

    Ok(index)
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

    let vector_index = if let Some(ref embs) = embeddings {
        if !embs.is_empty() && !embs[0].is_empty() {
            match build_vector_index(embs, embs[0].len()) {
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
    let matches = vi.search(query_embedding, limit).map_err(|e| format!("Vector search: {e}"))?;

    let mut results = Vec::new();
    for i in 0..matches.keys.len() {
        let key = matches.keys[i] as usize;
        let distance = matches.distances[i];
        // Cosine distance → similarity: sim = 1 - distance
        let similarity = 1.0 - distance;

        if key < cached.chunks.len() {
            let chunk = &cached.chunks[key];
            results.push(SearchResult {
                path: chunk.path.clone(),
                content: chunk.content.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                bm25_score: 0.0,
                vector_score: similarity,
                final_score: similarity * 100.0,
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

// ─── Embedding API ─────────────────────────────────────────────────────────

async fn gemini_embeddings(texts: &[String], api_key: &str) -> Result<Vec<Vec<f32>>, String> {
    let client = reqwest::Client::new();
    // Batch in groups of 100 (Gemini limit)
    let mut all_embeddings = Vec::new();
    for batch in texts.chunks(100) {
        let requests: Vec<serde_json::Value> = batch.iter().map(|t| {
            serde_json::json!({
                "model": "models/text-embedding-004",
                "content": { "parts": [{ "text": &t[..t.len().min(2000)] }] }
            })
        }).collect();

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/text-embedding-004:batchEmbedContents?key={api_key}"
        );

        let resp = client.post(&url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "requests": requests }))
            .send().await
            .map_err(|e| format!("Gemini embedding error: {e}"))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Gemini embedding failed: {body}"));
        }

        #[derive(Deserialize)]
        struct Resp { embeddings: Vec<Emb> }
        #[derive(Deserialize)]
        struct Emb { values: Vec<f32> }

        let result: Resp = resp.json().await.map_err(|e| format!("Parse: {e}"))?;
        all_embeddings.extend(result.embeddings.into_iter().map(|e| e.values));
    }
    Ok(all_embeddings)
}

async fn openai_embeddings(texts: &[String], api_key: &str) -> Result<Vec<Vec<f32>>, String> {
    let client = reqwest::Client::new();
    let resp = client.post("https://api.openai.com/v1/embeddings")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&serde_json::json!({
            "model": "text-embedding-3-small",
            "input": texts.iter().map(|t| &t[..t.len().min(2000)]).collect::<Vec<_>>(),
        }))
        .send().await
        .map_err(|e| format!("OpenAI embedding error: {e}"))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("OpenAI embedding failed: {body}"));
    }

    #[derive(Deserialize)]
    struct Resp { data: Vec<Item> }
    #[derive(Deserialize)]
    struct Item { embedding: Vec<f32> }

    let result: Resp = resp.json().await.map_err(|e| format!("Parse: {e}"))?;
    Ok(result.data.into_iter().map(|d| d.embedding).collect())
}

// ─── Tool implementation ───────────────────────────────────────────────────

pub struct CodeSearchTool {
    pub embedding_provider: Option<String>,
    pub embedding_api_key: Option<String>,
}

impl CodeSearchTool {
    pub fn new() -> Self {
        Self { embedding_provider: None, embedding_api_key: None }
    }

    pub fn with_embeddings(provider: String, api_key: String) -> Self {
        Self { embedding_provider: Some(provider), embedding_api_key: Some(api_key) }
    }
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
            let chunk_texts: Vec<String> = chunks.iter()
                .map(|c| c.content.chars().take(500).collect())
                .collect();

            // Optionally embed all chunks
            let embeddings = if let (Some(provider), Some(key)) = (&self.embedding_provider, &self.embedding_api_key) {
                if !key.is_empty() && !chunk_texts.is_empty() {
                    let result = match provider.as_str() {
                        "google" | "gemini" => gemini_embeddings(&chunk_texts, key).await,
                        "openai" => openai_embeddings(&chunk_texts, key).await,
                        _ => Err("Unknown provider".into()),
                    };
                    match result {
                        Ok(embs) => Some(embs),
                        Err(e) => {
                            tracing::warn!("Embedding failed, BM25 only: {e}");
                            None
                        }
                    }
                } else { None }
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
            if let (Some(provider), Some(key)) = (&self.embedding_provider, &self.embedding_api_key) {
                let query_emb = match provider.as_str() {
                    "google" | "gemini" => gemini_embeddings(&[input.query.clone()], key).await,
                    "openai" => openai_embeddings(&[input.query.clone()], key).await,
                    _ => Err("Unknown provider".into()),
                };
                match query_emb {
                    Ok(embs) if !embs.is_empty() => {
                        let cache = INDEX_CACHE.lock().unwrap();
                        let cached = cache.as_ref().unwrap();
                        let vec_results = vector_search(cached, &embs[0], VECTOR_CANDIDATES).unwrap_or_default();
                        drop(cache);
                        merge_results(bm25_results, vec_results, limit)
                    }
                    _ => { let mut r = bm25_results; r.truncate(limit); r }
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
