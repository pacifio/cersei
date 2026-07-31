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
    // B1: schemas cross the provider boundary only through `adapt_tools`.
    if !request.tools.is_empty() {
        let mut api_tools =
            crate::adapt::adapt_tools(&request.tools, crate::adapt::SchemaDialect::AnthropicNative);
        if let Some(last) = api_tools.last_mut() {
            last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
        }
        body["tools"] = serde_json::Value::Array(api_tools);
    }

    let mode = thinking_mode(model);

    if !request.stop_sequences.is_empty() {
        body["stop_sequences"] = serde_json::json!(request.stop_sequences);
    }

    // Thinking is resolved *before* temperature, because whether a thinking key
    // is emitted is half of what decides whether temperature is legal. Doing it
    // the other way round is what let the manual-model 400 through.
    //
    // A `thinking_budget` of `None` — or of 0, this codebase's "disable
    // thinking" sentinel (see `gemini.rs`) — means the caller does not want
    // extended thinking, so no `thinking` key is emitted in any mode.
    let requested_budget = thinking_budget.filter(|&budget| budget > 0);
    let thinking = match mode {
        // Any explicit `thinking` config is a 400 here, `{type:"disabled"}`
        // included — and thinking is on regardless. Emit nothing.
        ThinkingMode::AlwaysOn => None,
        // Depth is the model's call; `budget_tokens` would be a 400.
        // `display: "summarized"` is opt-in: the default is `"omitted"`, which
        // streams empty thinking blocks and would silently blank the thinking
        // output this agent already renders from `ThinkingDelta`.
        ThinkingMode::Adaptive => requested_budget
            .map(|_| serde_json::json!({ "type": "adaptive", "display": "summarized" })),
        ThinkingMode::Manual => requested_budget
            .and_then(|budget| clamp_thinking_budget(budget, request.max_tokens, model))
            .map(|budget| serde_json::json!({ "type": "enabled", "budget_tokens": budget })),
    };
    if let Some(thinking) = &thinking {
        body["thinking"] = thinking.clone();
    }

    // §10.5 #6: on models where the manual budget is rejected, the budget used
    // to be silently discarded and every effort level produced a byte-identical
    // body running at the API default (`high`). `output_config.effort` is the
    // adaptive-era replacement (GA, no beta header; legal values
    // low|medium|high|xhigh|max), so translate the requested budget into the
    // effort level it stands for. Manual models keep `budget_tokens` and get no
    // `output_config` — pre-4.6 models reject or ignore it.
    if matches!(mode, ThinkingMode::Adaptive | ThinkingMode::AlwaysOn) {
        if let Some(budget) = requested_budget {
            body["output_config"] =
                serde_json::json!({ "effort": effort_for_budget(budget) });
        }
    }

    // The request-level ban below is an *Anthropic* rule. Non-Claude ids arrive
    // through `ANTHROPIC_BASE_URL` gateways whose sampling behaviour this build
    // cannot know, and the same reasoning that makes `thinking_mode` default
    // them to the legacy shape applies here: leave them exactly as they were.
    let thinking_bans_temperature = thinking.is_some() && model.contains("claude");
    if mode.accepts_sampling_params(thinking_bans_temperature) {
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
    } else if request.temperature.is_some() {
        tracing::debug!(
            "dropping temperature for model '{model}': rejected with a 400 either \
             because this model dropped sampling parameters, or because extended \
             thinking is enabled on this request"
        );
    }

    // F-08: forced tool choice, requested per-turn by the runner's
    // no-tool-call nudge via `options.tool_choice = "required"`.
    //
    // Manual extended thinking (`{type:"enabled"}`) documents forced
    // tool_choice as a 400 — only `auto`/`none` are legal alongside it — so
    // thinking wins and the force is dropped there. Adaptive thinking has no
    // such restriction on the direct API (only Bedrock requires
    // thinking-disabled with a forced choice), so it passes through.
    let manual_thinking_emitted = thinking
        .as_ref()
        .is_some_and(|t| t["type"] == "enabled");
    if !request.tools.is_empty()
        && request.options.get::<String>("tool_choice").as_deref() == Some("required")
    {
        if manual_thinking_emitted {
            tracing::debug!(
                "dropping forced tool_choice for model '{model}': incompatible \
                 with manual extended thinking (only auto/none are accepted)"
            );
        } else {
            body["tool_choice"] = serde_json::json!({ "type": "any" });
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
pub(crate) enum ThinkingMode {
    /// Send no `thinking` key at all.
    AlwaysOn,
    /// `{type:"adaptive"}`; no budget, no sampling parameters.
    Adaptive,
    /// Legacy `{type:"enabled", budget_tokens:N}`; sampling parameters accepted.
    Manual,
}

impl ThinkingMode {
    /// Whether `temperature` / `top_p` / `top_k` may be sent on this request.
    ///
    /// There are **two independent bans**, and missing the second one is a live
    /// 400 on the direct-Anthropic default model:
    ///
    /// 1. *Model-level.* Adaptive-only and always-on models removed sampling
    ///    parameters altogether.
    /// 2. *Request-level.* **Any** model rejects a non-default temperature while
    ///    extended thinking is enabled. Verbatim, from the live API on
    ///    `claude-sonnet-4-6` — a `Manual` model:
    ///
    ///    > `temperature` may only be set to 1 when thinking is enabled.
    ///
    ///    So `Manual` is not a blanket permit; it is a permit only while no
    ///    `thinking` key is being sent. The original gate checked the model and
    ///    not the request, which fixed F-01's adaptive path and left every
    ///    `--effort`-driven run on 4.6-era models sending an illegal body.
    ///
    /// Temperature 1 is technically legal alongside thinking, but 1 is also the
    /// value the API uses when thinking is on, so dropping it is equivalent and
    /// avoids a float comparison deciding whether a request 400s.
    fn accepts_sampling_params(self, thinking_enabled: bool) -> bool {
        matches!(self, Self::Manual) && !thinking_enabled
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
pub(crate) fn thinking_mode(model: &str) -> ThinkingMode {
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

/// Translate a manual thinking budget into the adaptive API's effort level.
///
/// The budget is the only thinking signal this wire carries: abstract-cli maps
/// `--effort` to exactly 1024 (Low), 4096 (Medium), 8192 (High), or 32768
/// (Max) before it reaches the provider. Those four values map back to their
/// levels; arbitrary library-caller budgets land on the nearest level, with
/// cut points at the midpoints between adjacent canonical budgets. `xhigh`
/// is deliberately never produced — nothing on this wire expresses it, and
/// inventing it here would misreport what the caller asked for.
fn effort_for_budget(budget: u32) -> &'static str {
    match budget {
        0..=2559 => "low",       // canonical 1024; midpoint(1024, 4096) = 2560
        2560..=6143 => "medium", // canonical 4096; midpoint(4096, 8192) = 6144
        6144..=20479 => "high",  // canonical 8192; midpoint(8192, 32768) = 20480
        _ => "max",              // canonical 32768
    }
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
                // The signature must survive to be echoed back in history;
                // dropping it here is what left every echoed thinking block
                // with `signature: ""` (§10.5 #7).
                "signature_delta" => Some(StreamEvent::SignatureDelta {
                    index,
                    signature: delta["signature"].as_str().unwrap_or("").to_string(),
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

    /// §10.5 #7 wiring: the SSE reader must map `signature_delta` to an event
    /// rather than dropping it in the catch-all — the drop is what left every
    /// echoed thinking block with an empty signature.
    #[test]
    fn sse_signature_delta_is_parsed_not_dropped() {
        let raw = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"EqQBCgIY\"}}";
        match parse_sse_event(raw) {
            Some(StreamEvent::SignatureDelta { index, signature }) => {
                assert_eq!(index, 2);
                assert_eq!(signature, "EqQBCgIY");
            }
            other => panic!("signature_delta must parse to SignatureDelta, got {other:?}"),
        }
    }

    #[test]
    fn beta_header_omits_stale_interleaved_thinking_identifier() {
        // `interleaved-thinking-2025-04-14` is rejected by the current
        // Anthropic API with HTTP 400, breaking every request since this
        // header is sent unconditionally. See
        // https://github.com/pacifio/cersei/issues/20.
        assert!(!ANTHROPIC_BETA_HEADER.contains("interleaved-thinking-2025-04-14"));
        assert_eq!(ANTHROPIC_BETA_HEADER, "token-efficient-tools-2025-02-19");
    }

    // ─── B1 wiring ───────────────────────────────────────────────────────────
    //
    // `build_anthropic_body` is the Anthropic/Vertex serialization site, so a
    // body-shape assertion here binds the seam call directly. The OpenAI and
    // Gemini sites are bound in `tests/tool_body_shapes.rs`.

    /// The tool schemas in the body must be `adapt_tools` output — `$ref`
    /// inlined, `$schema`/`definitions` stripped — with the cache breakpoint
    /// still on the last tool.
    #[test]
    fn body_tools_are_adapted_and_cache_control_survives() {
        let mut r = CompletionRequest::new("claude-sonnet-5");
        r.max_tokens = 1024;
        r.messages = vec![Message::user("go")];
        let schemars_like = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "additionalProperties": false,
            "properties": { "range": { "$ref": "#/definitions/Range" } },
            "definitions": {
                "Range": { "type": "object", "properties": { "start": { "type": "integer" } } }
            }
        });
        r.tools = vec![
            ToolDefinition {
                name: "Read".to_string(),
                description: "Reads a file".to_string(),
                input_schema: schemars_like,
            },
            ToolDefinition {
                name: "Grep".to_string(),
                description: "Searches".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
            },
        ];

        let body = build_anthropic_body("claude-sonnet-5", &r, None, None);
        let schema = &body["tools"][0]["input_schema"];
        assert!(schema.get("$schema").is_none(), "{schema:#}");
        assert!(schema.get("definitions").is_none(), "{schema:#}");
        assert!(schema["properties"]["range"].get("$ref").is_none(), "{schema:#}");
        assert_eq!(
            schema["properties"]["range"]["properties"]["start"]["type"], "integer",
            "the $ref must be inlined in place: {schema:#}"
        );
        // Native is permissive: the author's constraints pass through.
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        // The cache breakpoint the seam must not displace.
        assert_eq!(
            body["tools"][1]["cache_control"],
            serde_json::json!({ "type": "ephemeral" }),
            "the last tool carries the cache breakpoint for the whole set"
        );
        assert!(body["tools"][0].get("cache_control").is_none());
    }

    // ─── F-A8: the system prompt is the cacheable prefix ─────────────────────

    /// The system string — project instructions before the dynamic boundary
    /// included — lands in one system block carrying the cache breakpoint.
    /// The CLI-side half (instructions appear exactly once, before the
    /// boundary) is bound in `abstract-cli/src/prompt.rs`; together they bind
    /// F-A8's request-body claim. Splitting the block *at* the boundary so
    /// the dynamic tail stops invalidating the prefix is P3.
    #[test]
    fn body_system_block_carries_the_cache_breakpoint() {
        let mut r = CompletionRequest::new("claude-sonnet-5");
        r.max_tokens = 1024;
        r.system =
            Some("INSTRUCTIONS_MARKER\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\ndynamic tail".into());
        r.messages = vec![Message::user("go")];

        let body = build_anthropic_body("claude-sonnet-5", &r, None, None);
        let system = body["system"].as_array().expect("system must be blocks");
        assert_eq!(system.len(), 1);
        assert_eq!(
            system[0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
        let text = system[0]["text"].as_str().unwrap();
        assert_eq!(text.matches("INSTRUCTIONS_MARKER").count(), 1);
        assert!(
            text.find("INSTRUCTIONS_MARKER").unwrap()
                < text.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap(),
            "instructions must precede the boundary in the cached block"
        );
    }

    // ─── F-08 wiring: forced tool choice ────────────────────────────────────

    fn request_with_tools(model: &str, tool_choice: Option<&str>) -> CompletionRequest {
        let mut r = CompletionRequest::new(model);
        // Large enough that the F-01 clamp keeps a 4096 manual budget intact.
        r.max_tokens = 8192;
        r.messages = vec![Message::user("go")];
        r.tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Reads a file".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        }];
        if let Some(choice) = tool_choice {
            r.options.set("tool_choice", choice);
        }
        r
    }

    /// The option maps to Anthropic's `{"type":"any"}` — and only when asked.
    #[test]
    fn tool_choice_required_maps_to_any_and_defaults_to_absent() {
        let asked = build_anthropic_body(
            "claude-sonnet-5",
            &request_with_tools("claude-sonnet-5", Some("required")),
            None,
            None,
        );
        assert_eq!(asked["tool_choice"], serde_json::json!({ "type": "any" }));

        let unasked = build_anthropic_body(
            "claude-sonnet-5",
            &request_with_tools("claude-sonnet-5", None),
            None,
            None,
        );
        assert!(
            unasked.get("tool_choice").is_none(),
            "no option ⇒ provider default (auto), no key: {unasked:#}"
        );
    }

    /// Adaptive thinking has no forced-tool-choice restriction on the direct
    /// API, so both keys coexist.
    #[test]
    fn forced_tool_choice_coexists_with_adaptive_thinking() {
        let model = "claude-sonnet-5";
        assert_eq!(thinking_mode(model), ThinkingMode::Adaptive);
        let body = build_anthropic_body(
            model,
            &request_with_tools(model, Some("required")),
            Some(4096),
            None,
        );
        assert_eq!(body["thinking"]["type"], "adaptive", "precondition");
        assert_eq!(body["tool_choice"], serde_json::json!({ "type": "any" }));
    }

    /// Manual extended thinking documents forced tool_choice as a 400 (only
    /// auto/none are legal alongside it) — thinking wins, the force is
    /// dropped, and the request survives.
    #[test]
    fn forced_tool_choice_is_dropped_under_manual_thinking() {
        let model = "claude-3-7-sonnet-20250219";
        assert_eq!(
            thinking_mode(model),
            ThinkingMode::Manual,
            "precondition: this test needs a manual-thinking model"
        );
        let body = build_anthropic_body(
            model,
            &request_with_tools(model, Some("required")),
            Some(4096),
            None,
        );
        assert_eq!(body["thinking"]["type"], "enabled", "precondition");
        assert!(
            body.get("tool_choice").is_none(),
            "forced tool_choice alongside manual thinking is a documented 400: {body:#}"
        );
    }

    /// Live positive: adaptive model + tools + `{"type":"any"}` is accepted.
    /// If this 400s, the F-08 mapping emits a shape the API does not take.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and ANTHROPIC_API_KEY set"]
    async fn live_forced_tool_choice_on_adaptive_model_is_accepted() {
        let model = live_model();
        let mut body = build_anthropic_body(
            &model,
            &request_with_tools(&model, Some("required")),
            None,
            None,
        );
        body["stream"] = serde_json::json!(false);
        assert_eq!(
            body["tool_choice"],
            serde_json::json!({ "type": "any" }),
            "precondition: the mapping should have emitted the forced form"
        );
        let Some((status, text)) = post_live(&body).await else {
            return;
        };
        eprintln!("forced tool_choice on adaptive → HTTP {status}");
        assert!(
            (200..300).contains(&status),
            "forced tool_choice was rejected with {status}: {text}"
        );
    }

    /// Live negative: manual thinking + forced tool_choice is the documented
    /// 400 the gate exists for. If this starts passing, the API widened and
    /// the manual-thinking suppression can be removed.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and ANTHROPIC_API_KEY set"]
    async fn live_forced_tool_choice_plus_manual_thinking_is_rejected() {
        let model = live_manual_model();
        assert_eq!(
            thinking_mode(&model),
            ThinkingMode::Manual,
            "live negative needs a manual-thinking model; '{model}' is not one. \
             Set CERSEI_LIVE_ANTHROPIC_MANUAL_MODEL."
        );
        // Build the pre-gate shape by hand: gate output never carries both.
        let mut body = build_anthropic_body(
            &model,
            &request_with_tools(&model, None),
            Some(4096),
            None,
        );
        assert_eq!(body["thinking"]["type"], "enabled", "precondition");
        body["tool_choice"] = serde_json::json!({ "type": "any" });
        body["stream"] = serde_json::json!(false);
        let Some((status, text)) = post_live(&body).await else {
            return;
        };
        eprintln!("manual thinking + forced tool_choice → HTTP {status}");
        assert_eq!(
            status, 400,
            "the docs say only auto/none are legal with manual thinking; if \
             this now passes, drop the suppression in build_anthropic_body: {text}"
        );
    }

    // ─── F-01 ────────────────────────────────────────────────────────────────
    //
    // Assertions below are written against the Anthropic API contract using
    // literal token counts, not this module's constants, so that retuning a
    // constant cannot silently move the bar the tests check.

    // ─── F-01, live ──────────────────────────────────────────────────────────
    //
    // Everything else in this module checks the *shape* of the body against
    // Anthropic's documented contract. That is what §10.2 of
    // TOOL-CALLING-RELIABILITY.md calls "Confirmed from primary docs": it proves
    // Cersei sends what the docs describe, not that the API agrees with the
    // docs. These three tests close that gap by asking the real API.
    //
    // Requires a key and costs a few tokens per test, so they are `#[ignore]`:
    //
    //   ANTHROPIC_API_KEY=sk-... cargo test -p cersei-provider --lib live_ \
    //       -- --ignored --nocapture
    //
    // Override the model with CERSEI_LIVE_ANTHROPIC_MODEL. The default must be a
    // model `thinking_mode` classifies `Adaptive`, or these assert nothing.

    /// Model used by the live tests. Must be adaptive-only for the negative
    /// cases to mean anything; asserted rather than assumed in each test.
    fn live_model() -> String {
        std::env::var("CERSEI_LIVE_ANTHROPIC_MODEL")
            .unwrap_or_else(|_| "claude-sonnet-5".to_string())
    }

    /// POST a body to the real Messages API; return (status, response text).
    /// Returns `None` when no key is present so the tests degrade to a skip
    /// rather than a failure, matching the other live tests in this workspace.
    async fn post_live(body: &serde_json::Value) -> Option<(u16, String)> {
        let key = match std::env::var("ANTHROPIC_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("ANTHROPIC_API_KEY not set — skipping.");
                return None;
            }
        };
        let base = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());

        let resp = reqwest::Client::new()
            .post(format!("{}/v1/messages", base.trim_end_matches('/')))
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await
            .expect("request to the Anthropic API failed to send");

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Some((status, text))
    }

    /// A minimal real request: one user turn, non-streaming, room for thinking.
    fn live_body(model: &str, thinking_budget: Option<u32>) -> serde_json::Value {
        let mut r = CompletionRequest::new(model);
        r.max_tokens = 2048;
        r.messages = vec![Message::user("Reply with the single word: ok")];
        let mut body = build_anthropic_body(model, &r, thinking_budget, None);
        // Non-streaming keeps the assertion about status, not SSE parsing.
        body["stream"] = serde_json::json!(false);
        body
    }

    /// F-A8's cacheability claim, measured: the system block Cersei builds
    /// carries a breakpoint the real API honors. The first call must create
    /// a cache entry (`cache_creation_input_tokens > 0`), and an identical
    /// second call must read it back (`cache_read_input_tokens > 0`). A
    /// per-run nonce keeps the first call from hitting a prior run's entry.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and ANTHROPIC_API_KEY set"]
    async fn live_system_prefix_is_created_then_read_from_cache() {
        let model = live_model();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Long enough to clear the API's minimum cacheable prefix (1024
        // tokens on sonnet-class models).
        let filler =
            "Project instructions live on the cacheable side of the prompt. ".repeat(200);
        let mut r = CompletionRequest::new(&model);
        r.max_tokens = 64;
        r.system = Some(format!("Session {nonce}. {filler}"));
        r.messages = vec![Message::user("Reply with the single word: ok")];
        let mut body = build_anthropic_body(&model, &r, None, None);
        body["stream"] = serde_json::json!(false);

        let Some((status1, text1)) = post_live(&body).await else {
            return;
        };
        assert!(
            (200..300).contains(&status1),
            "first call failed: {status1}: {text1}"
        );
        let usage1 = serde_json::from_str::<serde_json::Value>(&text1).unwrap()["usage"].clone();
        let created = usage1["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        assert!(
            created > 0,
            "first call should create the cache entry: {usage1:#}"
        );

        let Some((status2, text2)) = post_live(&body).await else {
            return;
        };
        assert!(
            (200..300).contains(&status2),
            "second call failed: {status2}: {text2}"
        );
        let usage2 = serde_json::from_str::<serde_json::Value>(&text2).unwrap()["usage"].clone();
        let read = usage2["cache_read_input_tokens"].as_u64().unwrap_or(0);
        assert!(
            read > 0,
            "identical second call should read the cached prefix: {usage2:#}"
        );
        eprintln!("cache_creation={created}, cache_read={read}");
    }

    /// The positive case: what the gate actually builds for an adaptive-only
    /// model is accepted. If this 400s, the gate is emitting a shape the API
    /// does not take and F-01 is not fixed.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and ANTHROPIC_API_KEY set"]
    async fn live_gate_output_is_accepted_by_the_real_api() {
        let model = live_model();
        assert_eq!(
            thinking_mode(&model),
            ThinkingMode::Adaptive,
            "live F-01 tests need an adaptive-only model; '{model}' is not one. \
             Set CERSEI_LIVE_ANTHROPIC_MODEL."
        );

        let body = live_body(&model, Some(4096));
        assert_eq!(
            body["thinking"],
            serde_json::json!({ "type": "adaptive", "display": "summarized" }),
            "precondition: the gate should have rewritten this to the adaptive form"
        );

        let Some((status, text)) = post_live(&body).await else {
            return;
        };
        eprintln!("gate output → HTTP {status}");
        assert!(
            (200..300).contains(&status),
            "the body Cersei builds for '{model}' was rejected with {status}: {text}"
        );
    }

    /// The load-bearing case: the *pre-gate* body — byte-identical except for
    /// the legacy manual thinking form Cersei sent before F-01 — is rejected.
    /// This is what turns "the docs say 4.7+ rejects it" into "this key, this
    /// model, this account rejects it", and proves the gate is load-bearing
    /// rather than decorative.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and ANTHROPIC_API_KEY set"]
    async fn live_pre_gate_manual_form_is_rejected_by_the_real_api() {
        let model = live_model();
        assert_eq!(
            thinking_mode(&model),
            ThinkingMode::Adaptive,
            "this test asserts a rejection that only adaptive-only models make"
        );

        let mut body = live_body(&model, Some(4096));
        // Exactly what `anthropic.rs:197-199` emitted before the gate landed.
        body["thinking"] = serde_json::json!({ "type": "enabled", "budget_tokens": 4096 });

        let Some((status, text)) = post_live(&body).await else {
            return;
        };
        eprintln!("pre-gate manual form → HTTP {status}: {text}");
        assert_eq!(
            status, 400,
            "F-01 claims the legacy manual thinking form is a 400 on '{model}'. \
             Got {status}. If this is a 2xx, the model still accepts the old form \
             and F-01's severity is overstated for it: {text}"
        );
    }

    /// The sampling half of the same gate. §10.5 #10 records this as the one
    /// F-01-adjacent claim that could not be sourced from primary docs and was
    /// therefore never coded against with confidence — "one live API call
    /// settles it". This is that call.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and ANTHROPIC_API_KEY set"]
    async fn live_sampling_params_are_rejected_on_adaptive_models() {
        let model = live_model();
        assert_eq!(thinking_mode(&model), ThinkingMode::Adaptive);

        let mut body = live_body(&model, Some(4096));
        // The gate drops `temperature` on these models. Put it back.
        body["temperature"] = serde_json::json!(0.3);

        let Some((status, text)) = post_live(&body).await else {
            return;
        };
        eprintln!("temperature on an adaptive model → HTTP {status}: {text}");
        assert_eq!(
            status, 400,
            "the gate drops `temperature` on adaptive models on the theory that it \
             is rejected. Got {status} — if this is a 2xx, dropping it is \
             unnecessary and `accepts_sampling_params` is over-restrictive: {text}"
        );
    }

    // ─── §10.5 #6: `--effort` must not be inert on adaptive models ───────────

    /// The original defect, stated directly: before the fix, every effort level
    /// produced a byte-identical body on adaptive models, silently running at
    /// the API default. Two different levels must now produce different bodies.
    #[test]
    fn distinct_effort_levels_produce_distinct_adaptive_bodies() {
        let low = build_anthropic_body("claude-sonnet-5", &req(CLI_MAX_TOKENS), Some(1024), None);
        let max = build_anthropic_body("claude-sonnet-5", &req(CLI_MAX_TOKENS), Some(32768), None);
        assert_ne!(
            low, max,
            "different effort levels must reach the wire differently; identical \
             bodies mean --effort is inert again"
        );
    }

    #[test]
    fn adaptive_models_translate_the_budget_into_output_config_effort() {
        // The four canonical budgets abstract-cli emits, one per effort level.
        for (budget, level) in [(1024, "low"), (4096, "medium"), (8192, "high"), (32768, "max")] {
            let body =
                build_anthropic_body("claude-opus-4-8", &req(CLI_MAX_TOKENS), Some(budget), None);
            assert_eq!(
                body["output_config"]["effort"], level,
                "budget {budget} stands for effort '{level}'"
            );
            // The manual form must still never appear on these models (F-01).
            assert!(body["thinking"].get("budget_tokens").is_none());
        }
    }

    #[test]
    fn always_on_models_get_effort_but_still_no_thinking_key() {
        let body = build_anthropic_body("claude-fable-5", &req(CLI_MAX_TOKENS), Some(32768), None);
        assert!(
            body.get("thinking").is_none(),
            "any explicit thinking value is a 400 on always-on models"
        );
        assert_eq!(body["output_config"]["effort"], "max");
    }

    #[test]
    fn manual_models_get_budget_tokens_and_no_output_config() {
        let body = build_anthropic_body("claude-sonnet-4-6", &req(CLI_MAX_TOKENS), Some(8192), None);
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
        assert!(
            body.get("output_config").is_none(),
            "pre-4.6 models don't take output_config.effort; the budget carries \
             the depth on the manual path"
        );
    }

    #[test]
    fn no_thinking_request_means_no_output_config() {
        for budget in [None, Some(0)] {
            let body = build_anthropic_body("claude-opus-4-8", &req(CLI_MAX_TOKENS), budget, None);
            assert!(
                body.get("output_config").is_none(),
                "budget {budget:?} means the caller did not ask for thinking; \
                 emitting an effort would override the API default unasked"
            );
        }
    }

    #[test]
    fn off_canonical_budgets_land_on_the_nearest_level() {
        for (budget, level) in [(1, "low"), (3000, "medium"), (10_000, "high"), (100_000, "max")] {
            assert_eq!(effort_for_budget(budget), level, "budget {budget}");
        }
    }

    /// A manual-thinking model with a caller temperature — the case the first
    /// version of the F-01 gate got wrong.
    ///
    /// The *adaptive* rejection reads "`temperature` may only be set to 1 when
    /// thinking is enabled **or** in adaptive mode", and "thinking is enabled"
    /// is the manual `{type:"enabled"}` form. The live API confirmed that
    /// broader reading on `claude-sonnet-4-6`, a `Manual` model, with a 400 —
    /// so `accepts_sampling_params` now takes the request into account and not
    /// just the model. This is the paired positive: what the corrected gate
    /// builds is accepted.
    fn live_manual_body(model: &str) -> serde_json::Value {
        let mut r = CompletionRequest::new(model);
        r.max_tokens = 2048;
        r.messages = vec![Message::user("Reply with the single word: ok")];
        r.temperature = Some(0.3);
        let mut body = build_anthropic_body(model, &r, Some(1024), None);
        body["stream"] = serde_json::json!(false);
        body
    }

    fn live_manual_model() -> String {
        std::env::var("CERSEI_LIVE_ANTHROPIC_MANUAL_MODEL")
            .unwrap_or_else(|_| "claude-sonnet-4-6".to_string())
    }

    #[tokio::test]
    #[ignore = "live API test; run with --ignored and ANTHROPIC_API_KEY set"]
    async fn live_manual_thinking_gate_output_is_accepted() {
        let model = live_manual_model();
        assert_eq!(
            thinking_mode(&model),
            ThinkingMode::Manual,
            "this test needs a manual-thinking model; '{model}' is not one. \
             Set CERSEI_LIVE_ANTHROPIC_MANUAL_MODEL."
        );

        let body = live_manual_body(&model);
        // The corrected gate keeps the manual thinking budget and drops the
        // temperature that would otherwise 400 next to it.
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(
            body.get("temperature").is_none(),
            "precondition: the gate must drop temperature while thinking is on"
        );

        let Some((status, text)) = post_live(&body).await else {
            return;
        };
        eprintln!("manual gate output → HTTP {status}");
        assert!(
            (200..300).contains(&status),
            "the body Cersei builds for '{model}' was rejected with {status}: {text}"
        );
    }

    /// The load-bearing negative: put the dropped temperature back and the same
    /// request 400s. This is what proves the drop is required rather than
    /// merely cautious — §10.5 #10, settled from the API.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and ANTHROPIC_API_KEY set"]
    async fn live_manual_thinking_plus_temperature_is_rejected() {
        let model = live_manual_model();
        assert_eq!(thinking_mode(&model), ThinkingMode::Manual);

        let mut body = live_manual_body(&model);
        body["temperature"] = serde_json::json!(0.3);

        let Some((status, text)) = post_live(&body).await else {
            return;
        };
        eprintln!("manual thinking + temperature 0.3 → HTTP {status}: {text}");
        assert_eq!(
            status, 400,
            "the gate drops temperature alongside manual thinking because the API \
             rejects the pair. Got {status} — if this is a 2xx the drop is \
             unnecessary and the gate is over-restrictive: {text}"
        );
    }

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
        let unrecognised_claude = [
            "claude-opus-4-20250514",   // Opus 4, dated snapshot
            "claude-sonnet-4@20250514", // Sonnet 4, Vertex @-versioned
            "claude-sonnet-4.5",        // dotted minor version
            "claude-haiku-4-5",         // no adaptive support at all
        ];
        let gateway = [
            "glm-4.6",          // ANTHROPIC_BASE_URL gateway
            "kimi-k2-thinking", // ANTHROPIC_BASE_URL gateway
        ];

        for model in unrecognised_claude.iter().chain(gateway.iter()) {
            let body = build_anthropic_body(model, &r, Some(8192), None);
            assert_eq!(
                body["thinking"],
                serde_json::json!({ "type": "enabled", "budget_tokens": 8192 }),
                "{model} is not a known adaptive-only model and must keep the manual shape"
            );
        }

        // Claude ids obey Anthropic's request-level rule: no temperature while
        // thinking is on, whether or not this build recognises the version.
        for model in unrecognised_claude {
            let body = build_anthropic_body(model, &r, Some(8192), None);
            assert!(
                body.get("temperature").is_none(),
                "{model} is a Claude id with thinking enabled, so temperature 400s"
            );
        }

        // Gateway ids are not Anthropic and their sampling rules are unknown, so
        // they keep exactly the behaviour they had before either gate existed.
        for model in gateway {
            let body = build_anthropic_body(model, &r, Some(8192), None);
            assert_eq!(
                body["temperature"],
                serde_json::json!(0.3f32),
                "{model} is a non-Claude gateway id; stripping temperature is a regression"
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
        // Manual models with extended thinking ON: the model-level ban does not
        // apply, but the request-level one does. Confirmed live on
        // claude-sonnet-4-6: "`temperature` may only be set to 1 when thinking
        // is enabled." This is the case the first version of this gate got
        // wrong — it asserted the opposite and shipped a guaranteed 400.
        for model in ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"] {
            let body = build_anthropic_body(model, &r, Some(8192), None);
            assert!(
                body["thinking"].is_object(),
                "precondition: {model} should have been given a manual thinking budget"
            );
            assert!(
                body.get("temperature").is_none(),
                "{model} 400s on a non-default temperature while thinking is enabled, \
                 got {:?}",
                body.get("temperature")
            );
        }
        // Manual models with thinking OFF: nothing bans temperature, so the
        // caller's value must survive. Compare against an `f32` literal — the
        // body widens `request.temperature` (f32) to f64, so `json!(0.3f64)`
        // would not compare equal to it.
        for model in ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"] {
            for budget in [None, Some(0)] {
                let body = build_anthropic_body(model, &r, budget, None);
                assert!(
                    body.get("thinking").is_none(),
                    "precondition: budget {budget:?} must mean no thinking key"
                );
                assert_eq!(
                    body["temperature"],
                    serde_json::json!(0.3f32),
                    "{model} accepts temperature when thinking is off; it must be preserved"
                );
            }
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
