use async_trait::async_trait;
use serde::Deserialize;

use crate::{EmbeddingError, EmbeddingProvider};

const DEFAULT_MODEL: &str = "text-embedding-004";
const DEFAULT_DIMENSIONS: usize = 768;
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
            .map_err(|_| EmbeddingError::Config(
                "GOOGLE_API_KEY or GEMINI_API_KEY must be set".into(),
            ))?;
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
    fn name(&self) -> &str { "gemini" }
    fn dimensions(&self) -> usize { self.dimensions }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!(
            "{API_BASE}/models/{}:batchEmbedContents?key={}",
            self.model, self.api_key
        );
        let model_path = format!("models/{}", self.model);
        let truncate = self.truncate_chars;

        let mut all = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH_LIMIT) {
            let requests: Vec<serde_json::Value> = batch
                .iter()
                .map(|t| {
                    let end = t.len().min(truncate);
                    serde_json::json!({
                        "model": &model_path,
                        "content": { "parts": [{ "text": &t[..end] }] }
                    })
                })
                .collect();

            let resp = self
                .client
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({ "requests": requests }))
                .send()
                .await?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(EmbeddingError::Api(format!("Gemini embedding failed: {body}")));
            }

            let parsed: Resp = resp
                .json()
                .await
                .map_err(|e| EmbeddingError::Parse(e.to_string()))?;
            all.extend(parsed.embeddings.into_iter().map(|e| e.values));
        }
        Ok(all)
    }
}

#[derive(Deserialize)]
struct Resp { embeddings: Vec<Emb> }
#[derive(Deserialize)]
struct Emb { values: Vec<f32> }
