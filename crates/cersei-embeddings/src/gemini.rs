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
                    let url = format!("{API_BASE}/models/{model}:embedContent?key={key}");
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
                            .header("Content-Type", "application/json")
                            .json(&body)
                            .send()
                            .await
                        {
                            Err(e) => {
                                last_err = Some(EmbeddingError::from(e));
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
                                            last_err =
                                                Some(EmbeddingError::Parse(e.to_string()));
                                            continue;
                                        }
                                    }
                                }
                                let body_text = resp.text().await.unwrap_or_default();
                                let retryable =
                                    status.as_u16() == 429 || status.is_server_error();
                                last_err = Some(EmbeddingError::Api(format!(
                                    "Gemini embedding failed ({status}): {body_text}"
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
#[derive(Deserialize)]
struct Emb {
    values: Vec<f32>,
}
