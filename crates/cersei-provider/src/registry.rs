//! Static registry of known LLM providers.
//!
//! Each entry contains the provider's API base URL, env var names for auth,
//! API format (Anthropic or OpenAI-compatible), and known models with
//! context windows and capabilities.

use crate::ProviderCapabilities;

/// API format used by a provider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApiFormat {
    /// Anthropic's native API format (different SSE events, system prompt handling).
    Anthropic,
    /// OpenAI-compatible `/v1/chat/completions` format (used by most providers).
    OpenAiCompatible,
    /// Google Gemini native `generateContent` API format.
    Google,
}

/// A known LLM provider.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub api_base: &'static str,
    pub env_keys: &'static [&'static str],
    pub api_format: ApiFormat,
    pub default_model: &'static str,
    pub models: &'static [ModelEntry],
}

/// A known model within a provider.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub id: &'static str,
    pub context_window: u64,
    pub capabilities: ProviderCapabilities,
}

impl ProviderEntry {
    /// Try to read an API key from the environment using this provider's env key list.
    pub fn api_key_from_env(&self) -> Option<String> {
        for key in self.env_keys {
            if let Ok(val) = std::env::var(key) {
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
        None
    }

    /// Whether this provider requires an API key (Ollama does not).
    pub fn requires_key(&self) -> bool {
        !self.env_keys.is_empty()
    }

    /// Get the context window for a model, falling back to a default.
    pub fn context_window(&self, model: &str) -> u64 {
        self.models
            .iter()
            .find(|m| m.id == model)
            .map(|m| m.context_window)
            .unwrap_or(128_000)
    }
}

// ─── Capabilities shorthand ────────────────────────────────────────────────

const FULL: ProviderCapabilities = ProviderCapabilities {
    streaming: true,
    tool_use: true,
    vision: true,
    thinking: false,
    system_prompt: true,
    caching: false,
};

const FULL_THINKING: ProviderCapabilities = ProviderCapabilities {
    streaming: true,
    tool_use: true,
    vision: true,
    thinking: true,
    system_prompt: true,
    caching: true,
};

const BASIC: ProviderCapabilities = ProviderCapabilities {
    streaming: true,
    tool_use: true,
    vision: false,
    thinking: false,
    system_prompt: true,
    caching: false,
};

// ─── Provider Registry ─────────────────────────────────────────────────────

pub static REGISTRY: &[ProviderEntry] = &[
    ProviderEntry {
        id: "anthropic",
        name: "Anthropic",
        api_base: "https://api.anthropic.com",
        env_keys: &["ANTHROPIC_API_KEY", "ANTHROPIC_KEY"],
        api_format: ApiFormat::Anthropic,
        default_model: "claude-sonnet-4-6",
        models: &[
            ModelEntry { id: "claude-opus-4-6", context_window: 200_000, capabilities: FULL_THINKING },
            ModelEntry { id: "claude-sonnet-4-6", context_window: 200_000, capabilities: FULL_THINKING },
            ModelEntry { id: "claude-haiku-4-5", context_window: 200_000, capabilities: FULL },
        ],
    },
    ProviderEntry {
        id: "openai",
        name: "OpenAI",
        api_base: "https://api.openai.com/v1",
        env_keys: &["OPENAI_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "gpt-4o",
        models: &[
            ModelEntry { id: "gpt-4o", context_window: 128_000, capabilities: FULL },
            ModelEntry { id: "gpt-4-turbo", context_window: 128_000, capabilities: FULL },
            ModelEntry { id: "o1", context_window: 200_000, capabilities: FULL },
            ModelEntry { id: "o3", context_window: 200_000, capabilities: FULL },
        ],
    },
    ProviderEntry {
        id: "google",
        name: "Google",
        api_base: "https://generativelanguage.googleapis.com/v1beta",
        env_keys: &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        api_format: ApiFormat::Google,
        default_model: "gemini-2.0-flash",
        models: &[
            ModelEntry { id: "gemini-2.0-flash", context_window: 1_000_000, capabilities: FULL },
            ModelEntry { id: "gemini-2.0-pro", context_window: 1_000_000, capabilities: FULL },
            ModelEntry { id: "gemini-1.5-pro", context_window: 2_000_000, capabilities: FULL },
            ModelEntry { id: "gemini-1.5-flash", context_window: 1_000_000, capabilities: FULL },
        ],
    },
    ProviderEntry {
        id: "mistral",
        name: "Mistral",
        api_base: "https://api.mistral.ai/v1",
        env_keys: &["MISTRAL_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "mistral-large-latest",
        models: &[
            ModelEntry { id: "mistral-large-latest", context_window: 128_000, capabilities: FULL },
            ModelEntry { id: "codestral-latest", context_window: 256_000, capabilities: BASIC },
        ],
    },
    ProviderEntry {
        id: "groq",
        name: "Groq",
        api_base: "https://api.groq.com/openai/v1",
        env_keys: &["GROQ_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "llama-3.1-70b-versatile",
        models: &[
            ModelEntry { id: "llama-3.1-70b-versatile", context_window: 128_000, capabilities: BASIC },
            ModelEntry { id: "llama-3.1-8b-instant", context_window: 128_000, capabilities: BASIC },
            ModelEntry { id: "mixtral-8x7b-32768", context_window: 32_768, capabilities: BASIC },
        ],
    },
    ProviderEntry {
        id: "deepseek",
        name: "DeepSeek",
        api_base: "https://api.deepseek.com/v1",
        env_keys: &["DEEPSEEK_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "deepseek-chat",
        models: &[
            ModelEntry { id: "deepseek-chat", context_window: 64_000, capabilities: FULL },
            ModelEntry { id: "deepseek-coder", context_window: 64_000, capabilities: BASIC },
        ],
    },
    ProviderEntry {
        id: "xai",
        name: "xAI",
        api_base: "https://api.x.ai/v1",
        env_keys: &["XAI_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "grok-2",
        models: &[
            ModelEntry { id: "grok-2", context_window: 128_000, capabilities: FULL },
        ],
    },
    ProviderEntry {
        id: "together",
        name: "Together",
        api_base: "https://api.together.xyz/v1",
        env_keys: &["TOGETHER_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        models: &[
            ModelEntry { id: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo", context_window: 128_000, capabilities: BASIC },
        ],
    },
    ProviderEntry {
        id: "fireworks",
        name: "Fireworks",
        api_base: "https://api.fireworks.ai/inference/v1",
        env_keys: &["FIREWORKS_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
        models: &[
            ModelEntry { id: "accounts/fireworks/models/llama-v3p1-70b-instruct", context_window: 128_000, capabilities: BASIC },
        ],
    },
    ProviderEntry {
        id: "perplexity",
        name: "Perplexity",
        api_base: "https://api.perplexity.ai",
        env_keys: &["PERPLEXITY_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "llama-3.1-sonar-large-128k-online",
        models: &[
            ModelEntry { id: "llama-3.1-sonar-large-128k-online", context_window: 128_000, capabilities: BASIC },
        ],
    },
    ProviderEntry {
        id: "cerebras",
        name: "Cerebras",
        api_base: "https://api.cerebras.ai/v1",
        env_keys: &["CEREBRAS_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "llama3.1-70b",
        models: &[
            ModelEntry { id: "llama3.1-70b", context_window: 128_000, capabilities: BASIC },
        ],
    },
    ProviderEntry {
        id: "ollama",
        name: "Ollama",
        api_base: "http://localhost:11434/v1",
        env_keys: &[],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "llama3.1",
        models: &[],
    },
    ProviderEntry {
        id: "openrouter",
        name: "OpenRouter",
        api_base: "https://openrouter.ai/api/v1",
        env_keys: &["OPENROUTER_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "anthropic/claude-3.5-sonnet",
        models: &[],
    },
    ProviderEntry {
        id: "cohere",
        name: "Cohere",
        api_base: "https://api.cohere.com/compatibility/v1",
        env_keys: &["COHERE_API_KEY", "CO_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "command-r-plus",
        models: &[
            ModelEntry { id: "command-r-plus", context_window: 128_000, capabilities: FULL },
            ModelEntry { id: "command-r", context_window: 128_000, capabilities: FULL },
            ModelEntry { id: "command-a", context_window: 256_000, capabilities: FULL },
        ],
    },
    ProviderEntry {
        id: "sambanova",
        name: "SambaNova",
        api_base: "https://api.sambanova.ai/v1",
        env_keys: &["SAMBANOVA_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "Meta-Llama-3.1-70B-Instruct",
        models: &[
            ModelEntry { id: "Meta-Llama-3.1-70B-Instruct", context_window: 128_000, capabilities: BASIC },
            ModelEntry { id: "Meta-Llama-3.1-405B-Instruct", context_window: 128_000, capabilities: BASIC },
        ],
    },
];

/// Look up a provider by ID.
pub fn lookup(provider_id: &str) -> Option<&'static ProviderEntry> {
    REGISTRY.iter().find(|e| e.id == provider_id)
}

/// All registered providers.
pub fn all() -> &'static [ProviderEntry] {
    REGISTRY
}

/// Providers that have valid auth configured in the environment.
pub fn available() -> Vec<&'static ProviderEntry> {
    REGISTRY
        .iter()
        .filter(|e| !e.requires_key() || e.api_key_from_env().is_some())
        .collect()
}
