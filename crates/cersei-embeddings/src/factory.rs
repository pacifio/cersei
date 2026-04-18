use crate::{EmbeddingError, EmbeddingProvider, GeminiEmbeddings, OpenAiEmbeddings};

/// Construct an [`EmbeddingProvider`] by inferring the provider family from
/// an LLM model string.
///
/// Detection rules (in order):
/// - starts with `gpt`, `o1`, `o3`, contains `openai`   → OpenAI embeddings
/// - contains `gemini` or `google`                       → Gemini embeddings
/// - anything else                                       → OpenAI embeddings (default)
///
/// The relevant API key is read from the environment:
/// - OpenAI: `OPENAI_API_KEY`
/// - Gemini: `GOOGLE_API_KEY` (falling back to `GEMINI_API_KEY`)
///
/// Returns `EmbeddingError::Config` if no key is available.
pub fn auto_from_model(model: &str) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
    let m = model.to_ascii_lowercase();
    let is_openai = m.contains("openai")
        || m.starts_with("gpt")
        || m.starts_with("o1")
        || m.starts_with("o3");
    let is_gemini = m.contains("gemini") || m.contains("google");

    if is_gemini && !is_openai {
        Ok(Box::new(GeminiEmbeddings::from_env()?))
    } else {
        Ok(Box::new(OpenAiEmbeddings::from_env()?))
    }
}
