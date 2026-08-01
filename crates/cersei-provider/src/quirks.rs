//! B2 — `ProviderQuirks` (TOOL-CALLING-RELIABILITY.md §6 Option B).
//!
//! Four fields, not twenty. The discipline that keeps this from becoming the
//! rejected `ModelProfile`: **a field exists only because omitting or
//! mis-setting it produces a provider error**, measured or documented:
//!
//! - `thinking` — the wrong thinking form is a 400 (F-01, live-verified).
//! - `temperature` — sampling params alongside adaptive/always-on thinking
//!   models are a 400 (F-A12, live-verified in §10.5 #10).
//! - `context_window` — overestimating silently truncates the prompt front
//!   on local providers (F-09).
//! - `dialect` — Gemini rejects schema keys OpenAI strict requires (Exp 3);
//!   the wire shape is per-dialect or the request dies (B1).
//!
//! Preferences do not qualify. Resolution happens once, in
//! `router.rs::build_provider`, from `(ApiFormat, model)` — no second
//! per-model table. The per-model thinking truth stays in
//! `anthropic::thinking_mode` (the live-verified F-01 gate); this module
//! *exposes* it at router time rather than duplicating it.

use crate::adapt::SchemaDialect;
use crate::anthropic::{thinking_mode, ThinkingMode};
use crate::registry::ApiFormat;

/// How this (provider, model) pair takes extended thinking — or doesn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingQuirk {
    /// Thinking is always on server-side; any explicit `thinking` key is a
    /// 400 (Fable/Mythos-class models).
    AlwaysOn,
    /// `{type:"adaptive"}` only; `budget_tokens` is a 400.
    Adaptive,
    /// Legacy `{type:"enabled", budget_tokens:N}` (clamped below
    /// `max_tokens`); the only form pre-4.7 models take.
    Manual,
    /// The wire format has no thinking key this runtime can set; a
    /// `thinking_budget` option is ignored on this path.
    Unsupported,
}

/// Whether sampling parameters survive on this (provider, model) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemperaturePolicy {
    /// `temperature` passes through.
    Free,
    /// `temperature` is a 400 regardless of the request (sampling parameters
    /// removed on adaptive-only / always-on Claude models).
    Forbidden,
}

/// The per-(provider, model) incompatibilities the provider API forces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderQuirks {
    pub thinking: ThinkingQuirk,
    pub temperature: TemperaturePolicy,
    /// Real and conservative — the number compaction budgets against and the
    /// Ollama path sends as `options.num_ctx` (F-09).
    pub context_window: u64,
    pub dialect: SchemaDialect,
}

impl ProviderQuirks {
    /// Resolve at router time. `api_format` is the branch
    /// `build_provider` already takes; `model` feeds the per-model gates.
    pub fn resolve(api_format: ApiFormat, model: &str) -> Self {
        match api_format {
            ApiFormat::Anthropic | ApiFormat::AnthropicVertex => {
                let mode = thinking_mode(model);
                let thinking = match mode {
                    ThinkingMode::AlwaysOn => ThinkingQuirk::AlwaysOn,
                    ThinkingMode::Adaptive => ThinkingQuirk::Adaptive,
                    ThinkingMode::Manual => ThinkingQuirk::Manual,
                };
                // Sampling parameters were removed on the same models that
                // dropped manual thinking; manual models still take them
                // (unless a thinking key is on the request — a per-request
                // concern that stays in `build_anthropic_body`).
                let temperature = match mode {
                    ThinkingMode::Manual => TemperaturePolicy::Free,
                    _ => TemperaturePolicy::Forbidden,
                };
                ProviderQuirks {
                    thinking,
                    temperature,
                    context_window: context_window_for_model(model),
                    dialect: SchemaDialect::AnthropicNative,
                }
            }
            ApiFormat::Google => ProviderQuirks {
                // `generationConfig.thinkingConfig.thinkingBudget` is a
                // manual budget in Gemini's spelling.
                thinking: ThinkingQuirk::Manual,
                temperature: TemperaturePolicy::Free,
                context_window: context_window_for_model(model),
                dialect: SchemaDialect::GeminiSubset,
            },
            ApiFormat::OpenAiCompatible => ProviderQuirks {
                thinking: ThinkingQuirk::Unsupported,
                temperature: TemperaturePolicy::Free,
                context_window: context_window_for_model(model),
                // Loose until a model is measured to accept strict mode —
                // `strict: true` with a schema the model's tools don't
                // satisfy is its own 400.
                dialect: SchemaDialect::OpenAiLoose,
            },
        }
    }
}

/// Context window size for a model — the single live truth (F-09).
///
/// The catch-all is conservative on purpose: an unknown id is most often a
/// local Ollama tag (`qwen2.5-coder:7b`, `deepseek-r1`, …) whose real window
/// is small. Overestimating silently truncates the prompt front;
/// underestimating merely compacts early.
pub fn context_window_for_model(model: &str) -> u64 {
    match model {
        m if m.contains("gpt-5") => 1_000_000,
        m if m.contains("gemini") => 1_000_000,
        m if m.starts_with("o1") || m.starts_with("o3") => 200_000,
        m if m.contains("opus") => 200_000,
        m if m.contains("sonnet") => 200_000,
        m if m.contains("haiku") => 200_000,
        m if m.contains("gpt-4o") => 128_000,
        m if m.contains("gpt-4-turbo") => 128_000,
        m if m.contains("gpt-4") => 8_192,
        m if m.contains("gpt-3.5") => 16_385,
        m if m.contains("llama") => 8_192,
        // Newer Claude ids (fable, mythos, …) don't contain opus/sonnet/haiku.
        m if m.contains("claude") => 200_000,
        _ => 8_192,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_adaptive_model_forbids_temperature_and_keeps_native_dialect() {
        let q = ProviderQuirks::resolve(ApiFormat::Anthropic, "claude-sonnet-5");
        assert_eq!(q.thinking, ThinkingQuirk::Adaptive);
        assert_eq!(q.temperature, TemperaturePolicy::Forbidden);
        assert_eq!(q.context_window, 200_000);
        assert_eq!(q.dialect, SchemaDialect::AnthropicNative);
    }

    #[test]
    fn anthropic_manual_model_keeps_sampling_params() {
        let q = ProviderQuirks::resolve(ApiFormat::Anthropic, "claude-3-7-sonnet-20250219");
        assert_eq!(q.thinking, ThinkingQuirk::Manual);
        assert_eq!(q.temperature, TemperaturePolicy::Free);
    }

    #[test]
    fn vertex_resolves_like_anthropic() {
        let a = ProviderQuirks::resolve(ApiFormat::Anthropic, "claude-sonnet-5");
        let v = ProviderQuirks::resolve(ApiFormat::AnthropicVertex, "claude-sonnet-5");
        assert_eq!(a, v, "Vertex reuses build_anthropic_body; quirks must agree");
    }

    #[test]
    fn gemini_gets_the_subset_dialect_and_manual_thinking() {
        let q = ProviderQuirks::resolve(ApiFormat::Google, "gemini-flash-lite-latest");
        assert_eq!(q.dialect, SchemaDialect::GeminiSubset);
        assert_eq!(q.thinking, ThinkingQuirk::Manual);
        assert_eq!(q.context_window, 1_000_000);
    }

    #[test]
    fn openai_compatible_is_loose_and_thinking_free() {
        let q = ProviderQuirks::resolve(ApiFormat::OpenAiCompatible, "qwen2.5-coder:7b");
        assert_eq!(q.dialect, SchemaDialect::OpenAiLoose);
        assert_eq!(q.thinking, ThinkingQuirk::Unsupported);
        assert_eq!(q.temperature, TemperaturePolicy::Free);
        // F-09: the conservative catch-all is part of the quirk.
        assert_eq!(q.context_window, 8_192);
    }
}
