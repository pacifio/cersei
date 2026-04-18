use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error: {0}")]
    Api(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Index error: {0}")]
    Index(String),

    #[error("Config error: {0}")]
    Config(String),
}
