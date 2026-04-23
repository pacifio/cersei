use async_trait::async_trait;
use serde::Deserialize;

use crate::{EmbeddingError, EmbeddingProvider};

const DEFAULT_MODEL: &str = "text-embedding-3-small";
const DEFAULT_DIMENSIONS: usize = 1536;
const DEFAULT_TRUNCATE: usize = 2000;
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI (and compatible) text embeddings.
///
/// Defaults: model `text-embedding-3-small` (1536 dimensions), base URL
/// `https://api.openai.com/v1`, per-text truncation at 2000 characters.
///
/// The `base_url` override supports Azure OpenAI, Ollama, and other
/// OpenAI-compatible endpoints.
pub struct OpenAiEmbeddings {
    api_key: String,
    model: String,
    dimensions: usize,
    truncate_chars: usize,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAiEmbeddings {
    /// Construct with an explicit API key, using all defaults.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: DEFAULT_MODEL.into(),
            dimensions: DEFAULT_DIMENSIONS,
            truncate_chars: DEFAULT_TRUNCATE,
            base_url: DEFAULT_BASE_URL.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Construct from `OPENAI_API_KEY`.
    pub fn from_env() -> Result<Self, EmbeddingError> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| EmbeddingError::Config("OPENAI_API_KEY must be set".into()))?;
        if key.is_empty() {
            return Err(EmbeddingError::Config("OPENAI_API_KEY is empty".into()));
        }
        Ok(Self::new(key))
    }

    /// Override the embedding model. Also call
    /// [`with_dimensions`](Self::with_dimensions) if the new model's vector
    /// size differs from 1536.
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

    /// Override the base URL — point at Azure, Ollama, or another
    /// OpenAI-compatible endpoint.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbeddings {
    fn name(&self) -> &str {
        "openai"
    }
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let truncate = self.truncate_chars;
        // Char-boundary-safe truncation. Raw byte slicing panics on
        // multi-byte UTF-8 (Spanish diacritics, emoji, smart quotes).
        let owned: Vec<String> = texts
            .iter()
            .map(|t| {
                if t.len() <= truncate {
                    t.clone()
                } else {
                    let mut end = truncate;
                    while end > 0 && !t.is_char_boundary(end) {
                        end -= 1;
                    }
                    t[..end].to_string()
                }
            })
            .collect();
        let inputs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();

        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&serde_json::json!({
                "model": self.model,
                "input": inputs,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(EmbeddingError::Api(format!(
                "OpenAI embedding failed: {body}"
            )));
        }

        let parsed: Resp = resp
            .json()
            .await
            .map_err(|e| EmbeddingError::Parse(e.to_string()))?;
        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }
}

#[derive(Deserialize)]
struct Resp {
    data: Vec<Item>,
}
#[derive(Deserialize)]
struct Item {
    embedding: Vec<f32>,
}
