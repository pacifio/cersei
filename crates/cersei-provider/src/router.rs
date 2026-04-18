//! Model router: parse `provider/model` strings and construct the right provider.
//!
//! ```rust,ignore
//! use cersei_provider::router;
//!
//! let (provider, model) = router::from_model_string("openai/gpt-4o")?;
//! let (provider, model) = router::from_model_string("groq/llama-3.1-70b-versatile")?;
//! let (provider, model) = router::from_model_string("gpt-4o")?; // auto-detect
//! ```

use crate::registry::{self, ApiFormat, ProviderEntry};
use crate::{Anthropic, Auth, Gemini, OpenAi, Provider};
use cersei_types::*;

/// Parse a model string and return a configured provider + resolved model name.
///
/// Accepts:
/// - `"provider/model"` — explicit routing (e.g., `"groq/llama-3.1-70b-versatile"`)
/// - `"model-name"` — auto-detect from known prefixes and env vars (e.g., `"gpt-4o"`)
///
/// Returns `(provider, model_name)` where `model_name` has the provider prefix stripped.
pub fn from_model_string(model: &str) -> Result<(Box<dyn Provider>, String)> {
    // "auto" — pick the first available *keyed* provider's default model.
    //
    // Local providers (Ollama, etc.) are skipped here on purpose: they need
    // explicit opt-in via `--model ollama/<model>` so the CLI never silently
    // starts talking to a daemon the user didn't ask for.
    if model == "auto" {
        let available = registry::available();
        let entry = available
            .iter()
            .find(|e| e.requires_key())
            .copied()
            .ok_or_else(|| {
                let all_keys: Vec<String> = registry::all()
                    .iter()
                    .flat_map(|e| e.env_keys.iter().map(|k| k.to_string()))
                    .collect();
                CerseiError::Auth(format!(
                    "No API keys found. Set one of: {}\n\nOr point at a local provider explicitly, e.g. --model ollama/llama3.1",
                    all_keys.join(", ")
                ))
            })?;
        let model_name = entry.default_model;
        let provider = build_provider(entry, model_name)?;
        return Ok((provider, model_name.to_string()));
    }

    if let Some((provider_id, model_name)) = model.split_once('/') {
        // Explicit: "anthropic/claude-sonnet-4-6"
        let entry = registry::lookup(provider_id).ok_or_else(|| {
            let known: Vec<&str> = registry::all().iter().map(|e| e.id).collect();
            CerseiError::Config(format!(
                "Unknown provider: '{}'. Known providers: {}",
                provider_id,
                known.join(", ")
            ))
        })?;
        let provider = build_provider(entry, model_name)?;
        Ok((provider, model_name.to_string()))
    } else {
        // Auto-detect: "gpt-4o" → openai
        let (entry, resolved) = auto_detect(model)?;
        let provider = build_provider(entry, resolved)?;
        Ok((provider, resolved.to_string()))
    }
}

/// List all providers that have valid auth configured.
pub fn available_providers() -> Vec<&'static ProviderEntry> {
    registry::available()
}

/// List all known providers.
pub fn all_providers() -> &'static [ProviderEntry] {
    registry::all()
}

// ─── Internal ──────────────────────────────────────────────────────────────

fn build_provider(entry: &ProviderEntry, model: &str) -> Result<Box<dyn Provider>> {
    match entry.api_format {
        ApiFormat::Anthropic => {
            let key = entry.api_key_from_env().ok_or_else(|| {
                CerseiError::Auth(format!(
                    "No API key for {}. Set {} in your environment.",
                    entry.name,
                    entry.env_keys.join(" or ")
                ))
            })?;
            Ok(Box::new(Anthropic::new(Auth::ApiKey(key))))
        }
        ApiFormat::Google => {
            let key = entry.api_key_from_env().ok_or_else(|| {
                CerseiError::Auth(format!(
                    "No API key for {}. Set {} in your environment.",
                    entry.name,
                    entry.env_keys.join(" or ")
                ))
            })?;
            let provider = Gemini::builder()
                .api_key(key)
                .model(model)
                .build()?;
            Ok(Box::new(provider))
        }
        ApiFormat::OpenAiCompatible => {
            let key = if entry.requires_key() {
                entry.api_key_from_env().ok_or_else(|| {
                    CerseiError::Auth(format!(
                        "No API key for {}. Set {} in your environment.",
                        entry.name,
                        entry.env_keys.join(" or ")
                    ))
                })?
            } else {
                // Ollama and other local providers don't need a key
                "no-key".to_string()
            };

            let provider = OpenAi::builder()
                .base_url(entry.api_base)
                .api_key(key)
                .model(model)
                .build()?;

            Ok(Box::new(provider))
        }
    }
}

/// Auto-detect provider from a bare model name.
fn auto_detect(model: &str) -> Result<(&'static ProviderEntry, &str)> {
    // 1. Check known model prefixes
    let prefix_match = match model {
        m if m.starts_with("claude-") => Some("anthropic"),
        m if m.starts_with("gpt-") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("gpt5") => Some("openai"),
        m if m.starts_with("gemini-") => Some("google"),
        m if m.starts_with("mistral-") || m.starts_with("codestral-") => Some("mistral"),
        m if m.starts_with("deepseek-") => Some("deepseek"),
        m if m.starts_with("grok-") => Some("xai"),
        m if m.starts_with("command-") => Some("cohere"),
        m if m.starts_with("llama") => {
            // llama models could be on Groq, Together, etc.
            // Prefer Groq if key is set, otherwise Together
            if std::env::var("GROQ_API_KEY").ok().filter(|k| !k.is_empty()).is_some() {
                Some("groq")
            } else if std::env::var("TOGETHER_API_KEY").ok().filter(|k| !k.is_empty()).is_some() {
                Some("together")
            } else {
                Some("ollama")
            }
        }
        _ => None,
    };

    if let Some(provider_id) = prefix_match {
        if let Some(entry) = registry::lookup(provider_id) {
            return Ok((entry, model));
        }
    }

    // 2. Fall back to first available provider
    let available = registry::available();
    if let Some(entry) = available.first() {
        return Ok((entry, model));
    }

    // 3. Nothing available
    let all_keys: Vec<String> = registry::all()
        .iter()
        .flat_map(|e| e.env_keys.iter().map(|k| k.to_string()))
        .collect();

    Err(CerseiError::Auth(format!(
        "Cannot detect provider for model '{}'. No API keys found.\n\nSet one of: {}",
        model,
        all_keys.join(", ")
    )))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_explicit_routing_unknown_provider() {
        let result = from_model_string("nonexistent/some-model");
        assert!(result.is_err());
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("nonexistent"), "Error should mention the provider name: {msg}");
            }
            Ok(_) => panic!("Expected error for unknown provider"),
        }
    }

    #[test]
    fn test_auto_detect_prefixes() {
        // These test auto_detect logic without requiring env vars
        let (entry, model) = auto_detect("claude-sonnet-4-6").unwrap_or_else(|_| {
            // If no key is set, it still identifies the provider
            (registry::lookup("anthropic").unwrap(), "claude-sonnet-4-6")
        });
        assert_eq!(entry.id, "anthropic");
        assert_eq!(model, "claude-sonnet-4-6");
    }

    #[test]
    fn test_registry_lookup() {
        assert!(registry::lookup("anthropic").is_some());
        assert!(registry::lookup("openai").is_some());
        assert!(registry::lookup("groq").is_some());
        assert!(registry::lookup("ollama").is_some());
        assert!(registry::lookup("nonexistent").is_none());
    }

    #[test]
    fn test_registry_lookup_new_providers() {
        assert!(registry::lookup("cohere").is_some());
        assert!(registry::lookup("sambanova").is_some());
    }

    #[test]
    fn test_google_native_format() {
        let entry = registry::lookup("google").unwrap();
        assert_eq!(entry.api_format, ApiFormat::Google);
        assert!(entry.api_base.contains("v1beta"));
        assert!(!entry.api_base.contains("openai"));
    }

    #[test]
    fn test_auto_detect_cohere() {
        let (entry, model) = auto_detect("command-r-plus").unwrap_or_else(|_| {
            (registry::lookup("cohere").unwrap(), "command-r-plus")
        });
        assert_eq!(entry.id, "cohere");
        assert_eq!(model, "command-r-plus");
    }

    #[test]
    fn test_ollama_no_key_required() {
        let entry = registry::lookup("ollama").unwrap();
        assert!(!entry.requires_key());
    }

    #[test]
    fn test_all_providers_count() {
        assert!(registry::all().len() >= 15);
    }

    #[test]
    fn test_provider_entry_context_window() {
        let entry = registry::lookup("anthropic").unwrap();
        assert_eq!(entry.context_window("claude-sonnet-4-6"), 200_000);
        assert_eq!(entry.context_window("unknown-model"), 128_000); // fallback
    }
}
