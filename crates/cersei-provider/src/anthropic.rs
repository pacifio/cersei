//! Anthropic provider: Claude API client with streaming SSE support.

use crate::*;
use cersei_types::*;
use futures::StreamExt;
use tokio::sync::mpsc;

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
// `interleaved-thinking-2025-04-14` is a stale beta identifier the current
// Anthropic API rejects with HTTP 400, breaking every request since this
// header was sent unconditionally. Extended thinking still works via the
// `thinking` body parameter, which needs no beta header. See
// https://github.com/pacifio/cersei/issues/20.
const ANTHROPIC_BETA_HEADER: &str = "token-efficient-tools-2025-02-19";

// ─── Anthropic provider ──────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct Anthropic {
    auth: Auth,
    base_url: String,
    default_model: String,
    thinking_budget: Option<u32>,
    max_retries: u32,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(auth: Auth) -> Self {
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| ANTHROPIC_API_BASE.to_string());
        Self {
            auth,
            base_url,
            default_model: "claude-sonnet-4-6".to_string(),
            thinking_budget: None,
            max_retries: 5,
            client: reqwest::Client::new(),
        }
    }

    /// Create from `ANTHROPIC_API_KEY` environment variable.
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| CerseiError::Auth("ANTHROPIC_API_KEY not set".into()))?;
        Ok(Self::new(Auth::ApiKey(key)))
    }

    pub fn builder() -> AnthropicBuilder {
        AnthropicBuilder::default()
    }

    async fn auth_headers(&self) -> Result<Vec<(String, String)>> {
        match &self.auth {
            Auth::ApiKey(key) => Ok(vec![("x-api-key".into(), key.clone())]),
            Auth::Bearer(token) => Ok(vec![("authorization".into(), format!("Bearer {}", token))]),
            Auth::OAuth { token, .. } => Ok(vec![(
                "authorization".into(),
                format!("Bearer {}", token.access_token),
            )]),
            Auth::Custom(provider) => {
                let (name, value) = provider.get_credentials().await?;
                Ok(vec![(name, value)])
            }
        }
    }
}

#[async_trait::async_trait]
impl Provider for Anthropic {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn context_window(&self, model: &str) -> u64 {
        match model {
            m if m.contains("opus") => 200_000,
            m if m.contains("sonnet") => 200_000,
            m if m.contains("haiku") => 200_000,
            _ => 200_000,
        }
    }

    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_use: true,
            vision: true,
            thinking: true,
            system_prompt: true,
            caching: true,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        let thinking_budget = request
            .options
            .get::<u32>("thinking_budget")
            .or(self.thinking_budget);
        // Direct Anthropic: include "model" in the body, no vertex version.
        let body = build_anthropic_body(&model, &request, thinking_budget, None);

        let url = format!("{}/v1/messages", self.base_url);
        let mut req_builder = self
            .client
            .post(&url)
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA_HEADER)
            .header("content-type", "application/json");
        for (name, value) in self.auth_headers().await? {
            req_builder = req_builder.header(&name, &value);
        }

        let http_request = req_builder.json(&body).build().map_err(CerseiError::Http)?;
        spawn_sse(self.client.clone(), http_request).await
    }
}

// ─── Shared request/stream helpers (reused by the Vertex provider) ─────────────

/// Build the Anthropic Messages request body.
///
/// - `model`: the model id, always required — it selects the thinking and
///   sampling shape (see [`thinking_mode`]). It is *emitted* in the body only on
///   the direct path; Vertex carries the model in the URL and takes
///   `anthropic_version` in the body instead.
/// - `vertex_version`: `None` for direct Anthropic, `Some(v)` for Vertex.
/// - Prompt caching: a `cache_control: {type: ephemeral}` breakpoint is placed
///   on the tool list and the system prompt (the stable prefix), so multi-turn
///   runs reuse the cached prefix. (Inspired by efficiency-focused agents like
///   vix/codex; implemented via Anthropic's native prompt caching.)
pub(crate) fn build_anthropic_body(
    model: &str,
    request: &CompletionRequest,
    thinking_budget: Option<u32>,
    vertex_version: Option<&str>,
) -> serde_json::Value {
    let api_messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
        .collect();

    let mut body = serde_json::json!({
        "max_tokens": request.max_tokens,
        "messages": api_messages,
        "stream": true,
    });
    match vertex_version {
        // Vertex: the model lives in the URL path, the body carries the version.
        Some(v) => body["anthropic_version"] = serde_json::Value::String(v.to_string()),
        // Direct: the body carries the model.
        None => body["model"] = serde_json::Value::String(model.to_string()),
    }

    // System prompt as a cacheable content block (stable prefix).
    if let Some(system) = &request.system {
        body["system"] = serde_json::json!([{
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" },
        }]);
    }

    // Tools, with a cache breakpoint on the last tool (caches the whole tool set).
    if !request.tools.is_empty() {
        let mut api_tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        if let Some(last) = api_tools.last_mut() {
            last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
        }
        body["tools"] = serde_json::Value::Array(api_tools);
    }

    let mode = thinking_mode(model);

    // `temperature` / `top_p` / `top_k` were removed on the same models that
    // dropped the manual thinking API, and return a 400 there.
    if mode.accepts_sampling_params() {
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
    } else if request.temperature.is_some() {
        tracing::debug!(
            "dropping temperature for model '{model}': sampling parameters are rejected \
             with a 400 on this model"
        );
    }

    if !request.stop_sequences.is_empty() {
        body["stop_sequences"] = serde_json::json!(request.stop_sequences);
    }

    // A `thinking_budget` of `None` — or of 0, this codebase's "disable
    // thinking" sentinel (see `gemini.rs`) — means the caller does not want
    // extended thinking, so no `thinking` key is emitted in any mode.
    let requested_budget = thinking_budget.filter(|&budget| budget > 0);
    match mode {
        // Any explicit `thinking` config is a 400 here, `{type:"disabled"}`
        // included — and thinking is on regardless. Emit nothing.
        ThinkingMode::AlwaysOn => {}
        // Depth is the model's call; `budget_tokens` would be a 400.
        // `display: "summarized"` is opt-in: the default is `"omitted"`, which
        // streams empty thinking blocks and would silently blank the thinking
        // output this agent already renders from `ThinkingDelta`.
        ThinkingMode::Adaptive => {
            if requested_budget.is_some() {
                body["thinking"] =
                    serde_json::json!({ "type": "adaptive", "display": "summarized" });
            }
        }
        ThinkingMode::Manual => {
            if let Some(budget) = requested_budget
                .and_then(|budget| clamp_thinking_budget(budget, request.max_tokens, model))
            {
                body["thinking"] =
                    serde_json::json!({ "type": "enabled", "budget_tokens": budget });
            }
        }
    }
    body
}

/// Anthropic's floor for `budget_tokens` on the manual extended-thinking API.
const MIN_THINKING_BUDGET: u32 = 1024;

/// Fraction of `max_tokens` held back for visible output when a thinking budget
/// has to be clamped (a quarter).
const OUTPUT_RESERVE_DIVISOR: u32 = 4;

/// How a given Claude model wants extended thinking configured.
///
/// Sourced from Anthropic's current extended-thinking / adaptive-thinking and
/// per-model migration docs:
/// - Manual `{type:"enabled", budget_tokens:N}` **returns a 400** on Opus 4.7
///   and later, Sonnet 5, and Fable 5 / Mythos 5. It is deprecated but still
///   functional on Opus 4.6 / Sonnet 4.6, and is the only form older models take.
/// - `temperature` / `top_p` / `top_k` were removed on that same set and 400 too.
/// - On Fable 5 / Mythos 5 thinking is always on and *every* explicit `thinking`
///   value is rejected, `{type:"disabled"}` included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkingMode {
    /// Send no `thinking` key at all.
    AlwaysOn,
    /// `{type:"adaptive"}`; no budget, no sampling parameters.
    Adaptive,
    /// Legacy `{type:"enabled", budget_tokens:N}`; sampling parameters accepted.
    Manual,
}

impl ThinkingMode {
    /// Whether this model still accepts `temperature` / `top_p` / `top_k`.
    fn accepts_sampling_params(self) -> bool {
        matches!(self, Self::Manual)
    }
}

/// Classify a model id into its thinking/sampling shape.
///
/// Matching is by substring so that router-prefixed ids
/// (`anthropic/claude-opus-4-8`) and dated snapshots (`claude-opus-4-8-20260115`,
/// Vertex's `claude-opus-4-8@20260115`) all land in the right family.
///
/// Only the models known to *reject* the legacy manual form are enumerated;
/// everything else defaults to `Manual`. That direction matters. The legacy form
/// is accepted by every Claude model older than Opus 4.7 **and** by the
/// Anthropic-Messages-compatible gateways reachable through `ANTHROPIC_BASE_URL`
/// (see [`Anthropic::new`]), whose model ids no table here can enumerate — so an
/// id this build does not recognise keeps exactly the behaviour it had before
/// this gate existed, and the gate can only ever fix a request, never break one
/// that worked. The cost is that a genuinely new adaptive-only Claude release
/// needs one line added below.
fn thinking_mode(model: &str) -> ThinkingMode {
    // Non-Claude ids arrive via an `ANTHROPIC_BASE_URL` gateway. Leave them on
    // the legacy shape rather than guessing a Claude-specific one at them.
    if !model.contains("claude") {
        return ThinkingMode::Manual;
    }
    // Thinking is always on and every explicit `thinking` value is rejected.
    if model.contains("fable") || model.contains("mythos") {
        return ThinkingMode::AlwaysOn;
    }
    // Families that reject `{type:"enabled", budget_tokens:N}` with a 400. Both
    // spellings of each minor version are listed because router ids use either
    // (`claude-opus-4-8`, `anthropic/claude-opus-4.8`).
    const ADAPTIVE_ONLY: &[&str] = &[
        "-4-7", "-4.7", // Opus 4.7
        "-4-8", "-4.8", // Opus 4.8
        // Sonnet 5. Note this cannot match `claude-sonnet-4-5`, whose
        // `sonnet-` is followed by `4`, and which does not support adaptive.
        "sonnet-5",
    ];
    if ADAPTIVE_ONLY.iter().any(|family| model.contains(family)) {
        return ThinkingMode::Adaptive;
    }
    ThinkingMode::Manual
}

/// Clamp a manual thinking budget into Anthropic's legal range for this
/// `max_tokens`, or return `None` when no legal value exists.
///
/// The API requires `1024 <= budget_tokens < max_tokens` — **both** bounds.
/// `--effort max` asks for 32768 while abstract-cli defaults `max_tokens` to
/// 16384, so the unclamped value is a guaranteed 400 before the first tool call.
/// Clamping to `max_tokens - 1` would satisfy the API and leave a single token to
/// answer with, so a quarter of the window is reserved for visible output
/// instead. Clamping is logged rather than silent — the run continues, but at a
/// different thinking depth than the effort level asked for.
///
/// A budget of 0 never reaches here: it means "no thinking" and is handled by the
/// caller. Any other sub-minimum budget is raised to the floor rather than
/// dropped, since the caller did ask for thinking.
fn clamp_thinking_budget(budget: u32, max_tokens: u32, model: &str) -> Option<u32> {
    let ceiling = max_tokens.saturating_sub(max_tokens / OUTPUT_RESERVE_DIVISOR);

    if ceiling < MIN_THINKING_BUDGET {
        tracing::warn!(
            "max_tokens {max_tokens} is too small to fit Anthropic's \
             {MIN_THINKING_BUDGET}-token minimum thinking budget for model '{model}'; \
             sending the request without extended thinking"
        );
        return None;
    }

    // `ceiling >= MIN_THINKING_BUDGET` is established above, so this is a valid
    // clamp range and the result always satisfies 1024 <= b <= ceiling < max_tokens.
    let clamped = budget.clamp(MIN_THINKING_BUDGET, ceiling);
    if clamped != budget {
        tracing::warn!(
            "thinking budget {budget} is outside Anthropic's legal range for model '{model}' \
             at max_tokens {max_tokens} (requires {MIN_THINKING_BUDGET} <= budget_tokens < \
             max_tokens, and the answer needs room); using {clamped}"
        );
    }
    Some(clamped)
}

/// Issue the request and, if the provider accepted it, spawn an SSE consumer
/// that parses Anthropic streaming events into a `CompletionStream`. Shared by
/// the direct and Vertex providers.
///
/// F-02: the response status is checked **here**, before anything is spawned,
/// so a non-2xx comes back as a typed `Err` from `complete()`. That is the only
/// place the runner's retry loop can see it — it guards `provider.complete()`
/// and nothing else. An earlier attempt reported the status as a stream event
/// instead, which put it *below* the retry loop: correctly typed, never
/// retried, session over.
pub(crate) async fn spawn_sse(
    client: reqwest::Client,
    request: reqwest::Request,
) -> Result<CompletionStream> {
    let response = client.execute(request).await.map_err(CerseiError::Http)?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let retry_after = crate::parse_retry_after(response.headers());
        let body = response.text().await.unwrap_or_default();
        return Err(CerseiError::from_http_status(status, retry_after, body));
    }

    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(pos) = buffer.find("\n\n") {
                        let event_str = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();
                        if let Some(event) = parse_sse_event(&event_str) {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error { message: e.to_string() }).await;
                    return;
                }
            }
        }
    });
    Ok(CompletionStream::new(rx))
}

// ─── SSE parser ──────────────────────────────────────────────────────────────

fn parse_sse_event(raw: &str) -> Option<StreamEvent> {
    let mut event_type = String::new();
    let mut data = String::new();

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = rest.trim().to_string();
        }
    }

    let json: serde_json::Value = serde_json::from_str(&data).ok()?;

    match event_type.as_str() {
        "message_start" => {
            let msg = &json["message"];
            Some(StreamEvent::MessageStart {
                id: msg["id"].as_str().unwrap_or("").to_string(),
                model: msg["model"].as_str().unwrap_or("").to_string(),
            })
        }
        "content_block_start" => {
            let index = json["index"].as_u64().unwrap_or(0) as usize;
            let block_type = json["content_block"]["type"]
                .as_str()
                .unwrap_or("text")
                .to_string();
            Some(StreamEvent::ContentBlockStart {
                index,
                block_type,
                id: json["content_block"]["id"].as_str().map(String::from),
                name: json["content_block"]["name"].as_str().map(String::from),
            })
        }
        "content_block_delta" => {
            let index = json["index"].as_u64().unwrap_or(0) as usize;
            let delta = &json["delta"];
            let delta_type = delta["type"].as_str().unwrap_or("");
            match delta_type {
                "text_delta" => Some(StreamEvent::TextDelta {
                    index,
                    text: delta["text"].as_str().unwrap_or("").to_string(),
                }),
                "input_json_delta" => Some(StreamEvent::InputJsonDelta {
                    index,
                    partial_json: delta["partial_json"].as_str().unwrap_or("").to_string(),
                }),
                "thinking_delta" => Some(StreamEvent::ThinkingDelta {
                    index,
                    thinking: delta["thinking"].as_str().unwrap_or("").to_string(),
                }),
                _ => None,
            }
        }
        "content_block_stop" => {
            let index = json["index"].as_u64().unwrap_or(0) as usize;
            Some(StreamEvent::ContentBlockStop { index })
        }
        "message_delta" => {
            let stop_reason = json["delta"]["stop_reason"].as_str().and_then(|s| match s {
                "end_turn" => Some(StopReason::EndTurn),
                "max_tokens" => Some(StopReason::MaxTokens),
                "tool_use" => Some(StopReason::ToolUse),
                "stop_sequence" => Some(StopReason::StopSequence),
                _ => None,
            });
            let usage = if let Some(u) = json["usage"].as_object() {
                Some(Usage {
                    input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    ..Default::default()
                })
            } else {
                None
            };
            Some(StreamEvent::MessageDelta { stop_reason, usage })
        }
        "message_stop" => Some(StreamEvent::MessageStop),
        "ping" => Some(StreamEvent::Ping),
        "error" => Some(StreamEvent::Error {
            message: json["error"]["message"]
                .as_str()
                .unwrap_or("Unknown error")
                .to_string(),
        }),
        _ => None,
    }
}

// ─── Builder ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct AnthropicBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    thinking_budget: Option<u32>,
    oauth_token: Option<OAuthToken>,
    max_retries: Option<u32>,
}

impl AnthropicBuilder {
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn thinking(mut self, budget_tokens: u32) -> Self {
        self.thinking_budget = Some(budget_tokens);
        self
    }

    pub fn oauth(mut self, token: OAuthToken) -> Self {
        self.oauth_token = Some(token);
        self
    }

    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = Some(n);
        self
    }

    pub fn build(self) -> Result<Anthropic> {
        let auth = if let Some(token) = self.oauth_token {
            Auth::OAuth {
                client_id: String::new(),
                token,
            }
        } else if let Some(key) = self.api_key {
            Auth::ApiKey(key)
        } else {
            return Err(CerseiError::Auth(
                "No API key or OAuth token provided. Set ANTHROPIC_API_KEY or use .oauth()".into(),
            ));
        };

        Ok(Anthropic {
            auth,
            base_url: self
                .base_url
                .unwrap_or_else(|| ANTHROPIC_API_BASE.to_string()),
            default_model: self
                .model
                .unwrap_or_else(|| "claude-sonnet-4-6".to_string()),
            thinking_budget: self.thinking_budget,
            max_retries: self.max_retries.unwrap_or(5),
            client: reqwest::Client::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic_vertex::VERTEX_VERSION;

    #[test]
    fn beta_header_omits_stale_interleaved_thinking_identifier() {
        // `interleaved-thinking-2025-04-14` is rejected by the current
        // Anthropic API with HTTP 400, breaking every request since this
        // header is sent unconditionally. See
        // https://github.com/pacifio/cersei/issues/20.
        assert!(!ANTHROPIC_BETA_HEADER.contains("interleaved-thinking-2025-04-14"));
        assert_eq!(ANTHROPIC_BETA_HEADER, "token-efficient-tools-2025-02-19");
    }

    // ─── F-01 ────────────────────────────────────────────────────────────────
    //
    // Assertions below are written against the Anthropic API contract using
    // literal token counts, not this module's constants, so that retuning a
    // constant cannot silently move the bar the tests check.

    /// `max_tokens` defaults to 16384 in abstract-cli (`config.rs`).
    const CLI_MAX_TOKENS: u32 = 16384;

    fn req(max_tokens: u32) -> CompletionRequest {
        let mut r = CompletionRequest::new("model-is-passed-separately");
        r.max_tokens = max_tokens;
        r
    }

    /// Half A: Anthropic requires `1024 <= budget_tokens < max_tokens`.
    /// `--effort max` asks for 32768 against a default `max_tokens` of 16384,
    /// which is a guaranteed 400 on turn 1 — pure arithmetic over two
    /// constants in this repo, no live call needed to see it.
    #[test]
    fn effort_max_budget_is_clamped_under_max_tokens() {
        let body = build_anthropic_body("claude-sonnet-4-6", &req(CLI_MAX_TOKENS), Some(32768), None);
        let budget = body["thinking"]["budget_tokens"]
            .as_u64()
            .expect("manual-thinking model must still carry budget_tokens");
        assert!(
            budget < CLI_MAX_TOKENS as u64,
            "budget_tokens {budget} must be strictly less than max_tokens {CLI_MAX_TOKENS}"
        );
        assert!(
            budget >= 1024,
            "budget_tokens {budget} must stay at or above Anthropic's 1024 minimum"
        );
        // A clamp to `max_tokens - 1` satisfies the API and still starves the
        // answer, so require real headroom for visible output.
        assert!(
            CLI_MAX_TOKENS as u64 - budget >= 1024,
            "clamped budget {budget} leaves under 1024 tokens of {CLI_MAX_TOKENS} for the answer"
        );
    }

    /// Half A, other direction: budgets that were already valid must not move.
    #[test]
    fn low_medium_high_budgets_are_left_alone() {
        for budget in [1024u32, 4096, 8192] {
            let body =
                build_anthropic_body("claude-sonnet-4-6", &req(CLI_MAX_TOKENS), Some(budget), None);
            assert_eq!(
                body["thinking"],
                serde_json::json!({ "type": "enabled", "budget_tokens": budget }),
                "valid budget {budget} must pass through unchanged"
            );
        }
    }

    /// Half A edge: when `max_tokens` is too small to fit even the 1024-token
    /// minimum budget, there is no legal `budget_tokens` — omit thinking rather
    /// than emit a body the API rejects.
    ///
    /// Note this exercises the *ceiling* branch (the window is too small), not
    /// the floor — see `sub_minimum_budget_is_raised_to_the_api_floor`.
    #[test]
    fn max_tokens_too_small_for_any_legal_budget_omits_thinking() {
        let body = build_anthropic_body("claude-sonnet-4-6", &req(512), Some(32768), None);
        assert!(
            body.get("thinking").is_none(),
            "no legal budget fits in max_tokens=512; thinking must be absent, got {:?}",
            body.get("thinking")
        );
    }

    /// Half A, the *other* bound: Anthropic requires `budget_tokens >= 1024`, so
    /// a caller-supplied budget under the floor is as much a 400 as one over the
    /// ceiling. Reachable from the public API via
    /// `Agent::builder().thinking_budget(512)` / `Anthropic::builder().thinking(512)`.
    #[test]
    fn sub_minimum_budget_is_raised_to_the_api_floor() {
        for budget in [1u32, 512, 1023] {
            let body =
                build_anthropic_body("claude-sonnet-4-6", &req(CLI_MAX_TOKENS), Some(budget), None);
            let emitted = body["thinking"]["budget_tokens"].as_u64().unwrap();
            assert_eq!(
                emitted, 1024,
                "budget {budget} is under Anthropic's 1024 floor and must be raised to it, \
                 got {emitted}"
            );
        }
    }

    /// A budget of 0 is this repo's "disable thinking" sentinel (see
    /// `gemini.rs`'s handling and `examples/gemini_vision_test.rs`). It must turn
    /// thinking *off*, not emit `budget_tokens: 0` (a 400) and not silently
    /// enable adaptive thinking.
    #[test]
    fn zero_budget_disables_thinking_in_every_mode() {
        for model in [
            "claude-sonnet-4-6", // Manual
            "claude-opus-4-8",   // Adaptive
            "claude-fable-5",    // AlwaysOn
        ] {
            let body = build_anthropic_body(model, &req(CLI_MAX_TOKENS), Some(0), None);
            assert!(
                body.get("thinking").is_none(),
                "budget 0 means 'no thinking' on {model}, got {:?}",
                body.get("thinking")
            );
        }
    }

    /// The classifier defaults to the legacy manual shape, so an id it does not
    /// recognise keeps the behaviour it had before this gate existed. This
    /// covers three ways an id misses the adaptive-only table: Claude 4.0's
    /// dated snapshots and Vertex's `@`-versioned form (neither carries a minor
    /// version), the dotted spelling of a minor version, and the non-Claude ids
    /// served by `ANTHROPIC_BASE_URL` gateways.
    #[test]
    fn unrecognised_ids_keep_the_legacy_manual_shape() {
        let mut r = req(CLI_MAX_TOKENS);
        r.temperature = Some(0.3);
        for model in [
            "claude-opus-4-20250514",   // Opus 4, dated snapshot
            "claude-sonnet-4@20250514", // Sonnet 4, Vertex @-versioned
            "claude-sonnet-4.5",        // dotted minor version
            "claude-haiku-4-5",         // no adaptive support at all
            "glm-4.6",                  // ANTHROPIC_BASE_URL gateway
            "kimi-k2-thinking",         // ANTHROPIC_BASE_URL gateway
        ] {
            let body = build_anthropic_body(model, &r, Some(8192), None);
            assert_eq!(
                body["thinking"],
                serde_json::json!({ "type": "enabled", "budget_tokens": 8192 }),
                "{model} is not a known adaptive-only model and must keep the manual shape"
            );
            assert_eq!(
                body["temperature"],
                serde_json::json!(0.3f32),
                "{model} still accepts temperature; stripping it is a regression"
            );
        }
    }

    /// Both spellings of the adaptive-only minor versions must classify, and a
    /// dated snapshot of one must too.
    #[test]
    fn adaptive_only_table_matches_both_spellings_and_snapshots() {
        for model in [
            "claude-opus-4.8",
            "claude-opus-4.7",
            "anthropic/claude-opus-4-8",
            "claude-opus-4-8@20260115",
            "claude-sonnet-5-20260115",
        ] {
            let body = build_anthropic_body(model, &req(CLI_MAX_TOKENS), Some(8192), None);
            assert_eq!(
                body["thinking"]["type"], "adaptive",
                "{model} rejects the manual form and must get adaptive"
            );
        }
    }

    /// Half B: `thinking: {type: "enabled", budget_tokens: N}` is rejected with
    /// a 400 on Opus 4.7 and later, Sonnet 5, and Fable 5. Those models take
    /// `{type: "adaptive"}` instead, with no budget.
    #[test]
    fn adaptive_only_models_never_get_a_manual_thinking_budget() {
        for model in ["claude-opus-4-8", "claude-opus-4-7", "claude-sonnet-5"] {
            let body = build_anthropic_body(model, &req(CLI_MAX_TOKENS), Some(8192), None);
            let thinking = body
                .get("thinking")
                .unwrap_or_else(|| panic!("{model} supports adaptive thinking; expected a key"));
            assert_eq!(
                thinking["type"], "adaptive",
                "{model} must use adaptive thinking, got {thinking:?}"
            );
            assert!(
                thinking.get("budget_tokens").is_none(),
                "{model} rejects budget_tokens with a 400, got {thinking:?}"
            );
        }
    }

    /// Half B: on Fable 5 / Mythos 5 thinking is always on and *any* explicit
    /// `thinking` config is a 400 — including `{type: "disabled"}`. The key must
    /// be absent, not null.
    #[test]
    fn always_on_thinking_models_get_no_thinking_key_at_all() {
        for model in ["claude-fable-5", "claude-mythos-5"] {
            let body = build_anthropic_body(model, &req(CLI_MAX_TOKENS), Some(8192), None);
            assert!(
                !body.as_object().unwrap().contains_key("thinking"),
                "{model} rejects any explicit thinking config; the key must be absent, got {:?}",
                body.get("thinking")
            );
        }
    }

    /// Half B, same gate: `temperature` / `top_p` / `top_k` are removed on the
    /// adaptive-only models and return a 400. Gating thinking but still sending
    /// temperature leaves `--effort low` and `--effort max` failing on turn 1
    /// for exactly the models Half B is about.
    #[test]
    fn sampling_params_are_dropped_only_where_they_are_rejected() {
        let mut r = req(CLI_MAX_TOKENS);
        r.temperature = Some(0.3); // EffortLevel::Low
        for model in ["claude-opus-4-8", "claude-opus-4-7", "claude-sonnet-5", "claude-fable-5"] {
            let body = build_anthropic_body(model, &r, Some(8192), None);
            assert!(
                body.get("temperature").is_none(),
                "{model} rejects temperature with a 400, got {:?}",
                body.get("temperature")
            );
        }
        // Models that still accept it must keep it. Compare against an `f32`
        // literal: the body widens `request.temperature` (f32) to f64, so
        // `json!(0.3f64)` would not compare equal to it.
        for model in ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"] {
            let body = build_anthropic_body(model, &r, Some(8192), None);
            assert_eq!(
                body["temperature"],
                serde_json::json!(0.3f32),
                "{model} accepts temperature; it must be preserved"
            );
        }
    }

    /// Adaptive thinking defaults to `display: "omitted"`, which streams
    /// thinking blocks with empty text. This agent renders thinking from
    /// `StreamEvent::ThinkingDelta`, so the summary is opted into explicitly —
    /// otherwise migrating off the manual API silently blanks that output.
    #[test]
    fn adaptive_thinking_opts_into_summarized_display() {
        let body = build_anthropic_body("claude-opus-4-8", &req(CLI_MAX_TOKENS), Some(8192), None);
        assert_eq!(body["thinking"]["display"], "summarized");
    }

    /// A caller that asked for no thinking budget must not have thinking turned
    /// on for it, on any model family.
    #[test]
    fn no_budget_requested_means_no_thinking_key() {
        for model in [
            "claude-opus-4-8",  // Adaptive
            "claude-sonnet-4-6", // Manual
            "claude-fable-5",   // AlwaysOn
        ] {
            let body = build_anthropic_body(model, &req(CLI_MAX_TOKENS), None, None);
            assert!(
                body.get("thinking").is_none(),
                "{model}: no budget was requested, so thinking must be absent, got {:?}",
                body.get("thinking")
            );
        }
    }

    /// Vertex shares the body builder, so it must share the gate. Its default
    /// model is `claude-opus-4-8` — adaptive-only — which is why F-01 reads as
    /// "Vertex's default model rejects every request Cersei sends".
    #[test]
    fn vertex_path_is_gated_on_the_same_model_rules() {
        let mut r = req(CLI_MAX_TOKENS);
        r.temperature = Some(1.0); // EffortLevel::Max
        let body =
            build_anthropic_body("claude-opus-4-8", &r, Some(32768), Some(VERTEX_VERSION));

        // Vertex shape is preserved: version in the body, model in the URL.
        assert_eq!(body["anthropic_version"], VERTEX_VERSION);
        assert!(
            body.get("model").is_none(),
            "Vertex takes the model in the URL path, not the body"
        );
        // ...and the gate applied.
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(
            body["thinking"].get("budget_tokens").is_none(),
            "Vertex's default model rejects budget_tokens too"
        );
        assert!(body.get("temperature").is_none());
    }

    /// The direct path keeps emitting `model` and no `anthropic_version`.
    #[test]
    fn direct_path_carries_model_and_no_vertex_version() {
        let body = build_anthropic_body("claude-sonnet-4-6", &req(CLI_MAX_TOKENS), None, None);
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert!(body.get("anthropic_version").is_none());
        assert!(
            body.get("thinking").is_none(),
            "no budget requested and manual mode: thinking must stay absent"
        );
    }
}
