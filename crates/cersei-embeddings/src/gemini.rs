use async_trait::async_trait;
use serde::Deserialize;

use crate::{EmbeddingError, EmbeddingProvider};

// `text-embedding-004` was retired on v1beta in early 2026. `gemini-embedding-001`
// is the current default; it returns 3072-dim vectors by default and supports
// Matryoshka truncation via the `outputDimensionality` request field if the
// caller wants a smaller vector (768 / 1536 / 3072).
const DEFAULT_MODEL: &str = "gemini-embedding-001";
const DEFAULT_DIMENSIONS: usize = 3072;
const DEFAULT_TRUNCATE: usize = 2000;
const BATCH_LIMIT: usize = 100;
const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google Gemini text embeddings.
///
/// Defaults: model `text-embedding-004` (768 dimensions), per-text truncation
/// at 2000 characters, 100-per-batch (the API limit).
pub struct GeminiEmbeddings {
    api_key: String,
    model: String,
    dimensions: usize,
    truncate_chars: usize,
    client: reqwest::Client,
}

impl GeminiEmbeddings {
    /// Construct with an explicit API key, using default model and dimensions.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: DEFAULT_MODEL.into(),
            dimensions: DEFAULT_DIMENSIONS,
            truncate_chars: DEFAULT_TRUNCATE,
            client: reqwest::Client::new(),
        }
    }

    /// Construct from `GOOGLE_API_KEY` or `GEMINI_API_KEY`.
    pub fn from_env() -> Result<Self, EmbeddingError> {
        let key = std::env::var("GOOGLE_API_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .map_err(|_| {
                EmbeddingError::Config("GOOGLE_API_KEY or GEMINI_API_KEY must be set".into())
            })?;
        if key.is_empty() {
            return Err(EmbeddingError::Config("Gemini API key is empty".into()));
        }
        Ok(Self::new(key))
    }

    /// Override the embedding model.
    ///
    /// If you use a model whose dimensionality differs from 768, call
    /// [`with_dimensions`](Self::with_dimensions) too.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override the declared vector dimensionality.
    pub fn with_dimensions(mut self, dims: usize) -> Self {
        self.dimensions = dims;
        self
    }

    /// Override the per-text character truncation limit.
    pub fn with_truncate_chars(mut self, chars: usize) -> Self {
        self.truncate_chars = chars;
        self
    }
}

#[async_trait]
impl EmbeddingProvider for GeminiEmbeddings {
    fn name(&self) -> &str {
        "gemini"
    }
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // `gemini-embedding-001` (and later) only support the singular
        // `embedContent` synchronous endpoint — the `batchEmbedContents`
        // endpoint was only on `text-embedding-004`, which was retired.
        // We loop with tokio::spawn so the in-flight requests run in
        // parallel up to the runtime's default budget.
        let truncate = self.truncate_chars;
        let target_dim = self.dimensions;

        use futures::stream::{self, StreamExt, TryStreamExt};

        // Bounded in-flight requests: cap at 20 parallel HTTP calls per batch.
        // Caller's outer concurrency multiplies this, but 20 per call keeps
        // any single ingest from saturating the connection pool.
        const IN_FLIGHT: usize = 20;

        let model = self.model.clone();
        let key = self.api_key.clone();
        let client = self.client.clone();

        let all: Vec<Vec<f32>> = stream::iter(texts.iter().cloned())
            .map(move |t| {
                let client = client.clone();
                let key = key.clone();
                let model = model.clone();
                async move {
                    // SECURITY: the API key goes in the `x-goog-api-key`
                    // header, NEVER in the URL query string. Query-string
                    // keys surface in every reqwest `Display` error, every
                    // traced span, every log line — leaks are unavoidable.
                    // Header auth keeps the URL (and any error mentioning it)
                    // secret-free. `redact_url_key` below is belt-and-braces
                    // for older logs that still contain `?key=…`.
                    let url = format!("{API_BASE}/models/{model}:embedContent");
                    let safe_url = url.clone();
                    // Char-boundary-safe truncation.
                    let mut end = t.len().min(truncate);
                    while end > 0 && !t.is_char_boundary(end) {
                        end -= 1;
                    }
                    let body = serde_json::json!({
                        "model": format!("models/{model}"),
                        "content": { "parts": [{ "text": &t[..end] }] },
                        // Matryoshka: request a smaller vector if target_dim
                        // is less than the native dimensionality.
                        "outputDimensionality": target_dim,
                    });
                    // Retry transient errors (network flaps, 5xx, 429) with
                    // exponential backoff. Keeps long-running benches alive
                    // through short internet drops.
                    const MAX_ATTEMPTS: u32 = 6;
                    let mut last_err: Option<EmbeddingError> = None;
                    for attempt in 0..MAX_ATTEMPTS {
                        if attempt > 0 {
                            // 500ms, 1s, 2s, 4s, 8s, 16s backoff.
                            let delay_ms = 500u64 * (1u64 << (attempt - 1).min(5));
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        }
                        match client
                            .post(&url)
                            .header("x-goog-api-key", &key)
                            .header("Content-Type", "application/json")
                            .json(&body)
                            .send()
                            .await
                        {
                            Err(e) => {
                                // IMPORTANT: reqwest's Display format for
                                // errors includes the full URL (`?key=...`).
                                // We construct a KEY-SAFE string manually.
                                last_err = Some(EmbeddingError::Api(format!(
                                    "Gemini embedding transport error ({safe_url}): {}",
                                    redact_errors(&e)
                                )));
                                continue;
                            }
                            Ok(resp) => {
                                let status = resp.status();
                                if status.is_success() {
                                    match resp.json::<SingleResp>().await {
                                        Ok(parsed) => {
                                            return Ok::<Vec<f32>, EmbeddingError>(
                                                parsed.embedding.values,
                                            );
                                        }
                                        Err(e) => {
                                            last_err = Some(EmbeddingError::Parse(
                                                redact_errors(&e),
                                            ));
                                            continue;
                                        }
                                    }
                                }
                                let body_text = resp.text().await.unwrap_or_default();
                                let retryable =
                                    status.as_u16() == 429 || status.is_server_error();
                                last_err = Some(EmbeddingError::Api(format!(
                                    "Gemini embedding failed ({status}, {safe_url}): {}",
                                    redact_url_key(&body_text)
                                )));
                                if !retryable {
                                    break;
                                }
                            }
                        }
                    }
                    Err(last_err.unwrap_or_else(|| {
                        EmbeddingError::Api("Gemini embedding: retries exhausted".into())
                    }))
                }
            })
            .buffered(IN_FLIGHT)
            .try_collect()
            .await?;

        Ok(all)
    }
}

#[derive(Deserialize)]
struct SingleResp {
    embedding: Emb,
}

/// Replace any `key=<value>` query-string segment with `key=REDACTED`.
///
/// The Gemini REST API takes the API key in the URL query string. `reqwest`'s
/// default error formatter includes the URL, which means any transient network
/// error would otherwise leak the key into logs, result files, panic messages,
/// and anywhere else the error string is persisted. This helper is applied to
/// any string (URL, response body, error `Display`) that might contain one.
pub(crate) fn redact_url_key(s: &str) -> String {
    // Match `key=<chars that are URL-safe but not `&` or whitespace>`.
    // We don't pull in a regex dep for one pattern — a manual scan is fine.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for `key=` at byte position i.
        if i + 4 <= bytes.len() && &bytes[i..i + 4] == b"key=" {
            out.push_str("key=REDACTED");
            i += 4;
            // Skip until a delimiter: &, whitespace, or quote.
            while i < bytes.len() {
                let b = bytes[i];
                if b == b'&' || b == b' ' || b == b'\n' || b == b'\r' || b == b'"' || b == b'\t' {
                    break;
                }
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn redact_errors<E: std::fmt::Display>(e: &E) -> String {
    redact_url_key(&e.to_string())
}

#[cfg(test)]
mod redact_tests {
    use super::redact_url_key;

    #[test]
    fn strips_key_from_query_string() {
        let raw = "GET https://generativelanguage.googleapis.com/v1beta/models/x:embedContent?key=AIzaSy123 failed";
        let redacted = redact_url_key(raw);
        assert!(!redacted.contains("AIzaSy123"));
        assert!(redacted.contains("key=REDACTED"));
    }

    #[test]
    fn preserves_non_key_text() {
        assert_eq!(redact_url_key("hello world"), "hello world");
    }

    #[test]
    fn handles_key_followed_by_ampersand() {
        let s = redact_url_key("?key=ABC&foo=bar");
        assert_eq!(s, "?key=REDACTED&foo=bar");
    }
}
#[derive(Deserialize)]
struct Emb {
    values: Vec<f32>,
}
