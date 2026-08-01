//! Static registry of known LLM providers.
//!
//! Each entry contains the provider's API base URL, env var names for auth,
//! API format (Anthropic or OpenAI-compatible), and known models with
//! context windows.


/// API format used by a provider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApiFormat {
    /// Anthropic's native API format (different SSE events, system prompt handling).
    Anthropic,
    /// Anthropic models served via Google Vertex AI (Model Garden).
    AnthropicVertex,
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
}

impl ProviderEntry {
    /// Try to read an API key from the environment using this provider's env key list.
    ///
    /// The value is trimmed: launchers that reconstruct a key via command
    /// substitution (`KEY="$(cat ...)"`) commonly leave a trailing newline,
    /// which otherwise produces a confusing `401` rather than a clean error.
    /// Whitespace-only values are treated as absent.
    pub fn api_key_from_env(&self) -> Option<String> {
        for key in self.env_keys {
            if let Ok(val) = std::env::var(key) {
                let val = val.trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
        None
    }

    /// Resolve the API base URL, honoring an environment override.
    ///
    /// For OpenAI-compatible providers this makes custom/proxy endpoints
    /// (DeepSeek, OpenRouter, local gateways, etc.) first-class: set
    /// `<PROVIDER>_BASE_URL` — e.g. `OPENAI_BASE_URL`, `DEEPSEEK_BASE_URL` —
    /// to redirect requests. `OPENAI_API_BASE` is accepted as a legacy alias
    /// for the `openai` provider. Falls back to the registered default.
    pub fn resolved_api_base(&self) -> String {
        let primary = format!("{}_BASE_URL", self.id.to_uppercase());
        let mut candidates = vec![primary.as_str()];
        if self.id == "openai" {
            candidates.push("OPENAI_API_BASE");
        }
        for key in candidates {
            if let Ok(val) = std::env::var(key) {
                let val = val.trim().trim_end_matches('/');
                if !val.is_empty() {
                    return val.to_string();
                }
            }
        }
        self.api_base.to_string()
    }

    /// Whether this provider requires an API key (Ollama does not).
    pub fn requires_key(&self) -> bool {
        !self.env_keys.is_empty()
    }

    /// Whether a local provider (one with no `env_keys`, e.g. Ollama) is
    /// actually reachable right now. Does a 200ms TCP probe against the
    /// host:port parsed out of `api_base`. Returns `true` when the probe
    /// succeeds, `false` otherwise. Providers that *do* require a key
    /// return `true` unconditionally (their availability is gated on the
    /// env var, not connectivity).
    pub fn is_reachable(&self) -> bool {
        if self.requires_key() {
            return true;
        }
        let host_port = extract_host_port(self.api_base);
        let Some(host_port) = host_port else {
            return false;
        };
        use std::net::ToSocketAddrs;
        let addrs: Vec<std::net::SocketAddr> = match host_port.to_socket_addrs() {
            Ok(it) => it.collect(),
            Err(_) => return false,
        };
        addrs.into_iter().any(|addr| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200))
                .is_ok()
        })
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
            ModelEntry {
                id: "claude-opus-4-6",
                context_window: 200_000,
            },
            ModelEntry {
                id: "claude-sonnet-4-6",
                context_window: 200_000,
            },
            ModelEntry {
                id: "claude-haiku-4-5",
                context_window: 200_000,
            },
        ],
    },
    ProviderEntry {
        id: "vertex",
        name: "Anthropic via Google Vertex AI",
        api_base: "https://aiplatform.googleapis.com",
        // Used only for availability/auto-detect. Actual auth is resolved by the
        // provider (service-account JSON / VERTEX_ACCESS_TOKEN / gcloud).
        env_keys: &["VERTEX_PROJECT_ID"],
        api_format: ApiFormat::AnthropicVertex,
        default_model: "claude-opus-4-8",
        models: &[
            ModelEntry {
                id: "claude-opus-4-8",
                context_window: 200_000,
            },
            ModelEntry {
                id: "claude-sonnet-4-6",
                context_window: 200_000,
            },
            ModelEntry {
                id: "claude-haiku-4-5",
                context_window: 200_000,
            },
        ],
    },
    ProviderEntry {
        id: "openai",
        name: "OpenAI",
        api_base: "https://api.openai.com/v1",
        env_keys: &["OPENAI_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "gpt-5.4-2026-03-05",
        models: &[
            ModelEntry {
                id: "gpt-5.4-2026-03-05",
                context_window: 1_000_000,
            },
            ModelEntry {
                id: "gpt-5.3-chat-latest",
                context_window: 1_000_000,
            },
            ModelEntry {
                id: "gpt-5.3-chat",
                context_window: 1_000_000,
            },
            ModelEntry {
                id: "gpt-5.3-codex",
                context_window: 1_000_000,
            },
            ModelEntry {
                id: "gpt-5-chat",
                context_window: 1_000_000,
            },
            ModelEntry {
                id: "gpt-4o",
                context_window: 128_000,
            },
            ModelEntry {
                id: "gpt-4-turbo",
                context_window: 128_000,
            },
            ModelEntry {
                id: "o1",
                context_window: 200_000,
            },
            ModelEntry {
                id: "o3",
                context_window: 200_000,
            },
            ModelEntry {
                id: "o3-pro",
                context_window: 200_000,
            },
        ],
    },
    ProviderEntry {
        id: "google",
        name: "Google",
        api_base: "https://generativelanguage.googleapis.com/v1beta",
        env_keys: &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        api_format: ApiFormat::Google,
        default_model: "gemini-3.1-pro-preview",
        models: &[
            ModelEntry {
                id: "gemini-3.1-pro-preview",
                context_window: 2_000_000,
            },
            ModelEntry {
                id: "gemini-3.0-flash",
                context_window: 1_000_000,
            },
            ModelEntry {
                id: "gemini-2.0-flash",
                context_window: 1_000_000,
            },
            ModelEntry {
                id: "gemini-2.0-pro",
                context_window: 1_000_000,
            },
            ModelEntry {
                id: "gemini-1.5-pro",
                context_window: 2_000_000,
            },
            ModelEntry {
                id: "gemini-1.5-flash",
                context_window: 1_000_000,
            },
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
            ModelEntry {
                id: "mistral-large-latest",
                context_window: 128_000,
            },
            ModelEntry {
                id: "codestral-latest",
                context_window: 256_000,
            },
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
            ModelEntry {
                id: "llama-3.1-70b-versatile",
                context_window: 128_000,
            },
            ModelEntry {
                id: "llama-3.1-8b-instant",
                context_window: 128_000,
            },
            ModelEntry {
                id: "mixtral-8x7b-32768",
                context_window: 32_768,
            },
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
            ModelEntry {
                id: "deepseek-chat",
                context_window: 64_000,
            },
            ModelEntry {
                id: "deepseek-reasoner",
                context_window: 64_000,
            },
            ModelEntry {
                id: "deepseek-coder",
                context_window: 64_000,
            },
        ],
    },
    ProviderEntry {
        id: "xai",
        name: "xAI",
        api_base: "https://api.x.ai/v1",
        env_keys: &["XAI_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "grok-2",
        models: &[ModelEntry {
            id: "grok-2",
            context_window: 128_000,
        }],
    },
    ProviderEntry {
        id: "together",
        name: "Together",
        api_base: "https://api.together.xyz/v1",
        env_keys: &["TOGETHER_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        models: &[ModelEntry {
            id: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
            context_window: 128_000,
        }],
    },
    ProviderEntry {
        id: "fireworks",
        name: "Fireworks",
        api_base: "https://api.fireworks.ai/inference/v1",
        env_keys: &["FIREWORKS_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
        models: &[ModelEntry {
            id: "accounts/fireworks/models/llama-v3p1-70b-instruct",
            context_window: 128_000,
        }],
    },
    ProviderEntry {
        id: "perplexity",
        name: "Perplexity",
        api_base: "https://api.perplexity.ai",
        env_keys: &["PERPLEXITY_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "llama-3.1-sonar-large-128k-online",
        models: &[ModelEntry {
            id: "llama-3.1-sonar-large-128k-online",
            context_window: 128_000,
        }],
    },
    ProviderEntry {
        id: "cerebras",
        name: "Cerebras",
        api_base: "https://api.cerebras.ai/v1",
        env_keys: &["CEREBRAS_API_KEY"],
        api_format: ApiFormat::OpenAiCompatible,
        default_model: "llama3.1-70b",
        models: &[ModelEntry {
            id: "llama3.1-70b",
            context_window: 128_000,
        }],
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
            ModelEntry {
                id: "command-r-plus",
                context_window: 128_000,
            },
            ModelEntry {
                id: "command-r",
                context_window: 128_000,
            },
            ModelEntry {
                id: "command-a",
                context_window: 256_000,
            },
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
            ModelEntry {
                id: "Meta-Llama-3.1-70B-Instruct",
                context_window: 128_000,
            },
            ModelEntry {
                id: "Meta-Llama-3.1-405B-Instruct",
                context_window: 128_000,
            },
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

/// Providers that have valid auth configured in the environment **and** — for
/// local providers without an API key (e.g. Ollama) — are actually reachable
/// via a quick TCP probe.
///
/// The probe prevents `from_model_string("auto")` from silently picking Ollama
/// when the daemon is not running, which was causing the CLI to default to
/// `llama3.1` on machines without any LLM installed.
pub fn available() -> Vec<&'static ProviderEntry> {
    REGISTRY
        .iter()
        .filter(|e| {
            if e.requires_key() {
                e.api_key_from_env().is_some()
            } else {
                e.is_reachable()
            }
        })
        .collect()
}

/// Extract a `host:port` string from an http(s) URL for TCP probing.
fn extract_host_port(api_base: &str) -> Option<String> {
    let trimmed = api_base
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let authority = trimmed.split('/').next()?;
    if authority.contains(':') {
        Some(authority.to_string())
    } else {
        // default ports based on scheme
        let port = if api_base.starts_with("https://") {
            443
        } else {
            80
        };
        Some(format!("{authority}:{port}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_resolves_native_base_by_default() {
        let entry = lookup("deepseek").unwrap();
        std::env::remove_var("DEEPSEEK_BASE_URL");
        let base = entry.resolved_api_base();
        assert_eq!(base, "https://api.deepseek.com/v1");
        assert!(!base.contains("api.openai.com"));
    }

    #[test]
    fn openai_base_url_override_redirects_target() {
        // Regression: `OPENAI_BASE_URL` must be honored so the request never
        // silently lands on api.openai.com when the user points elsewhere.
        let entry = lookup("openai").unwrap();
        std::env::set_var("OPENAI_BASE_URL", "https://api.deepseek.com/v1/");
        let base = entry.resolved_api_base();
        std::env::remove_var("OPENAI_BASE_URL");
        assert_eq!(base, "https://api.deepseek.com/v1"); // trailing slash trimmed
        assert!(!base.contains("api.openai.com"));
    }

    #[test]
    fn api_key_is_trimmed_and_blank_is_absent() {
        let entry = lookup("deepseek").unwrap();
        // A launcher leaving a trailing newline must not corrupt the key.
        std::env::set_var("DEEPSEEK_API_KEY", "sk-abc123\n");
        assert_eq!(entry.api_key_from_env().as_deref(), Some("sk-abc123"));
        // Whitespace-only is treated as no key set.
        std::env::set_var("DEEPSEEK_API_KEY", "   ");
        assert_eq!(entry.api_key_from_env(), None);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
}
