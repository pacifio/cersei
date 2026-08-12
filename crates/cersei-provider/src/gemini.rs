//! Google Gemini provider: native Gemini API client with streaming support.
//!
//! Uses Google's `generateContent` API directly rather than the OpenAI-compatible
//! shim, enabling access to native Gemini features like safety settings,
//! grounding, and proper multimodal support.

use crate::*;
use cersei_types::*;
use futures::StreamExt;
use tokio::sync::mpsc;

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Normalize Gemini's `usageMetadata`. Google documents
/// `promptTokenCount` as the total effective prompt (cached content included),
/// while `cachedContentTokenCount` is the cached subset.
fn normalize_usage_metadata(prompt: u64, output: u64, cached: u64) -> Usage {
    let cached = cached.min(prompt);
    Usage {
        input_tokens: prompt - cached,
        output_tokens: output,
        cache_read_input_tokens: cached,
        ..Default::default()
    }
}

// ─── Gemini provider ────────────────────────────────────────────────────────

pub struct Gemini {
    api_key: String,
    base_url: String,
    default_model: String,
    client: reqwest::Client,
}

impl Gemini {
    pub fn new(api_key: impl Into<String>) -> Self {
        let base_url = std::env::var("GEMINI_BASE_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| GEMINI_API_BASE.to_string());
        Self {
            api_key: api_key.into(),
            base_url,
            default_model: "gemini-3.1-pro-preview".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create from `GOOGLE_API_KEY` or `GEMINI_API_KEY` environment variable.
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("GOOGLE_API_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .map_err(|_| CerseiError::Auth("GOOGLE_API_KEY or GEMINI_API_KEY not set".into()))?;
        Ok(Self::new(key))
    }

    pub fn builder() -> GeminiBuilder {
        GeminiBuilder::default()
    }
}

#[async_trait::async_trait]
impl Provider for Gemini {
    fn name(&self) -> &str {
        "google"
    }

    fn context_window(&self, model: &str) -> u64 {
        match model {
            m if m.contains("gemini-3.1") => 2_000_000,
            m if m.contains("gemini-3.0") => 1_000_000,
            m if m.contains("gemini-2.0") => 1_000_000,
            m if m.contains("gemini-1.5-pro") => 2_000_000,
            m if m.contains("gemini-1.5-flash") => 1_000_000,
            _ => 1_000_000,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        // Build a map of tool_use_id → tool_name from conversation history
        let tool_name_map: std::collections::HashMap<String, String> = request
            .messages
            .iter()
            .flat_map(|m| match &m.content {
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolUse { id, name, .. } = b {
                            Some((id.clone(), name.clone()))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>(),
                _ => vec![],
            })
            .collect();

        // Build Gemini-native contents array
        let mut contents: Vec<serde_json::Value> = Vec::new();

        for msg in &request.messages {
            match msg.role {
                Role::User => {
                    let mut parts: Vec<serde_json::Value> = Vec::new();

                    if let MessageContent::Blocks(blocks) = &msg.content {
                        for block in blocks {
                            match block {
                                ContentBlock::Text { text } => {
                                    parts.push(serde_json::json!({ "text": text }));
                                }
                                ContentBlock::Image { source } => {
                                    if let Some(part) = gemini_media_part(
                                        source.media_type.as_deref(),
                                        source.data.as_deref(),
                                        source.url.as_deref(),
                                    ) {
                                        parts.push(part);
                                    }
                                }
                                ContentBlock::Document { source, .. } => {
                                    if let Some(part) = gemini_media_part(
                                        source.media_type.as_deref(),
                                        source.data.as_deref(),
                                        source.url.as_deref(),
                                    ) {
                                        parts.push(part);
                                    }
                                }
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    ..
                                } => {
                                    // Gemini requires the function NAME, not the call ID
                                    let func_name = tool_name_map
                                        .get(tool_use_id)
                                        .cloned()
                                        .unwrap_or_else(|| tool_use_id.clone());
                                    let content_str = match content {
                                        ToolResultContent::Text(s) => s.clone(),
                                        ToolResultContent::Blocks(blocks) => blocks
                                            .iter()
                                            .filter_map(|b| {
                                                if let ContentBlock::Text { text } = b {
                                                    Some(text.as_str())
                                                } else {
                                                    None
                                                }
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                    };
                                    parts.push(serde_json::json!({
                                        "functionResponse": {
                                            "name": func_name,
                                            "response": { "content": content_str },
                                        }
                                    }));
                                }
                                _ => {}
                            }
                        }
                    } else {
                        parts.push(serde_json::json!({ "text": msg.get_all_text() }));
                    }

                    if !parts.is_empty() {
                        contents.push(serde_json::json!({
                            "role": "user",
                            "parts": parts,
                        }));
                    }
                }
                Role::Assistant => {
                    let mut parts: Vec<serde_json::Value> = Vec::new();

                    if let MessageContent::Blocks(blocks) = &msg.content {
                        for block in blocks {
                            match block {
                                ContentBlock::Text { text } => {
                                    parts.push(serde_json::json!({ "text": text }));
                                }
                                ContentBlock::ToolUse { id, name, input } => {
                                    // Extract fc_id and thoughtSignature from encoded tool_id
                                    // Format: "gemini-tool-N::fc_id::thoughtSignature" or "gemini-tool-N"
                                    let segments: Vec<&str> = id.splitn(3, "::").collect();
                                    let mut fc = serde_json::json!({
                                        "name": name,
                                        "args": input,
                                    });
                                    let mut part_obj = serde_json::Map::new();
                                    if segments.len() >= 3 {
                                        // Has fc_id and thoughtSignature
                                        fc["id"] =
                                            serde_json::Value::String(segments[1].to_string());
                                        part_obj.insert("functionCall".to_string(), fc);
                                        part_obj.insert(
                                            "thoughtSignature".to_string(),
                                            serde_json::Value::String(segments[2].to_string()),
                                        );
                                    } else {
                                        part_obj.insert("functionCall".to_string(), fc);
                                    }
                                    parts.push(serde_json::Value::Object(part_obj));
                                }
                                _ => {}
                            }
                        }
                    } else {
                        parts.push(serde_json::json!({ "text": msg.get_all_text() }));
                    }

                    if !parts.is_empty() {
                        contents.push(serde_json::json!({
                            "role": "model",
                            "parts": parts,
                        }));
                    }
                }
                Role::System => {
                    // System messages handled separately via systemInstruction
                }
            }
        }

        // Build request body
        let mut body = serde_json::json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": request.max_tokens,
            },
        });

        // System instruction (Gemini's equivalent of system prompt). The
        // boundary marker is a client-side cache hint; Gemini caching is
        // automatic, so keep the literal string off the wire.
        if let Some(system) = &request.system {
            let text = system.replace(SYSTEM_PROMPT_DYNAMIC_BOUNDARY, "");
            body["systemInstruction"] = serde_json::json!({
                "parts": [{ "text": text }],
            });
        }

        if let Some(temp) = request.temperature {
            body["generationConfig"]["temperature"] = serde_json::json!(temp);
        }

        if !request.stop_sequences.is_empty() {
            body["generationConfig"]["stopSequences"] = serde_json::json!(request.stop_sequences);
        }

        // Optional thinking budget (gemini-2.5+). `thinking_budget = 0` disables
        // dynamic thinking, which otherwise silently consumes maxOutputTokens.
        if let Some(budget) = request.options.get::<i32>("thinking_budget") {
            body["generationConfig"]["thinkingConfig"] =
                serde_json::json!({ "thinkingBudget": budget });
        }

        // Tool declarations. B1: schemas cross the provider boundary only
        // through `adapt_tools` — Gemini rejects `$schema`/`$ref`/
        // `definitions`/`additionalProperties`, and the rejection kills the
        // whole request (Exp 1/3, TOOL-CALLING-RELIABILITY.md §7.0).
        if !request.tools.is_empty() {
            let function_declarations = crate::adapt::adapt_tools(
                &request.tools,
                crate::adapt::SchemaDialect::GeminiSubset,
            );
            body["tools"] = serde_json::json!([{
                "functionDeclarations": function_declarations,
            }]);

            // F-08: forced tool choice, requested per-turn by the runner's
            // no-tool-call nudge. Gemini's spelling is mode ANY.
            if request.options.get::<String>("tool_choice").as_deref() == Some("required") {
                body["toolConfig"] = serde_json::json!({
                    "functionCallingConfig": { "mode": "ANY" }
                });
            }
        }

        // Safety settings: use least restrictive defaults to avoid unexpected blocks
        body["safetySettings"] = serde_json::json!([
            { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "BLOCK_ONLY_HIGH" },
        ]);

        // SECURITY: never put the API key in the URL. Use the
        // `x-goog-api-key` header so that reqwest's error `Display` (which
        // prints the URL) cannot leak the secret into logs or error-wrapped
        // output.
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url, model
        );

        let req = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .build()
            .map_err(CerseiError::Http)?;

        // F-02: await the response and check its status *before* spawning, so a
        // non-2xx returns as a typed `Err` from `complete()`. The runner's retry
        // loop guards `provider.complete()` and nothing else — a status reported
        // from inside the spawned reader lands below that loop, where it can only
        // end the session.
        let response = self.client.execute(req).await.map_err(CerseiError::Http)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let retry_after = crate::parse_retry_after(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(CerseiError::from_http_status(status, retry_after, body));
        }

        let (tx, rx) = mpsc::channel(256);

        tokio::spawn(async move {
            let _ = tx
                .send(StreamEvent::MessageStart {
                    id: String::new(),
                    model: String::new(),
                    usage: None,
                })
                .await;

            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut block_index: usize = 0;
            let mut total_input_tokens: u64 = 0;
            let mut total_output_tokens: u64 = 0;
            let mut cached_content_tokens: u64 = 0;
            let mut saw_function_calls = false;

            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        while let Some(pos) = buffer.find("\n") {
                            let line = buffer[..pos].to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if let Some(data) = line.strip_prefix("data: ") {
                                let data = data.trim();
                                if data.is_empty() {
                                    continue;
                                }

                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                    // Extract usage metadata
                                    if let Some(metadata) = json.get("usageMetadata") {
                                        total_input_tokens = metadata
                                            .get("promptTokenCount")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(total_input_tokens);
                                        total_output_tokens = metadata
                                            .get("candidatesTokenCount")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(total_output_tokens);
                                        // `promptTokenCount` includes the
                                        // tokens served from cached content;
                                        // they are billed at the cache-read
                                        // rate, not the full input rate.
                                        cached_content_tokens = metadata
                                            .get("cachedContentTokenCount")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(cached_content_tokens);
                                    }

                                    // Process candidates
                                    if let Some(candidates) =
                                        json.get("candidates").and_then(|c| c.as_array())
                                    {
                                        for candidate in candidates {
                                            if let Some(parts) = candidate
                                                .get("content")
                                                .and_then(|c| c.get("parts"))
                                                .and_then(|p| p.as_array())
                                            {
                                                for part in parts {
                                                    if let Some(text) =
                                                        part.get("text").and_then(|t| t.as_str())
                                                    {
                                                        let _ = tx
                                                            .send(StreamEvent::ContentBlockStart {
                                                                index: block_index,
                                                                block_type: "text".into(),
                                                                id: None,
                                                                name: None,
                                                            })
                                                            .await;
                                                        let _ = tx
                                                            .send(StreamEvent::TextDelta {
                                                                index: block_index,
                                                                text: text.to_string(),
                                                            })
                                                            .await;
                                                        let _ = tx
                                                            .send(StreamEvent::ContentBlockStop {
                                                                index: block_index,
                                                            })
                                                            .await;
                                                        block_index += 1;
                                                    }

                                                    if let Some(fc) = part.get("functionCall") {
                                                        saw_function_calls = true;
                                                        let name = fc
                                                            .get("name")
                                                            .and_then(|n| n.as_str())
                                                            .unwrap_or("")
                                                            .to_string();
                                                        let args = fc
                                                            .get("args")
                                                            .cloned()
                                                            .unwrap_or(serde_json::Value::Object(
                                                                Default::default(),
                                                            ));
                                                        // Capture thoughtSignature (sibling of functionCall at part level, Gemini 3.1+)
                                                        let thought_sig = part
                                                            .get("thoughtSignature")
                                                            .and_then(|s| s.as_str())
                                                            .unwrap_or("");
                                                        // Capture functionCall.id if present
                                                        let fc_id = fc
                                                            .get("id")
                                                            .and_then(|s| s.as_str())
                                                            .unwrap_or("");
                                                        // Encode both in tool_id for roundtrip
                                                        let tool_id = if thought_sig.is_empty() {
                                                            format!("gemini-tool-{}", block_index)
                                                        } else {
                                                            format!(
                                                                "gemini-tool-{}::{}::{}",
                                                                block_index, fc_id, thought_sig
                                                            )
                                                        };

                                                        let _ = tx
                                                            .send(StreamEvent::ContentBlockStart {
                                                                index: block_index,
                                                                block_type: "tool_use".into(),
                                                                id: Some(tool_id),
                                                                name: Some(name),
                                                            })
                                                            .await;
                                                        let _ = tx
                                                            .send(StreamEvent::InputJsonDelta {
                                                                index: block_index,
                                                                partial_json:
                                                                    serde_json::to_string(&args)
                                                                        .unwrap_or_default(),
                                                            })
                                                            .await;
                                                        let _ = tx
                                                            .send(StreamEvent::ContentBlockStop {
                                                                index: block_index,
                                                            })
                                                            .await;
                                                        block_index += 1;
                                                    }
                                                }
                                            }

                                            // Check finish reason
                                            let finish_reason = candidate
                                                .get("finishReason")
                                                .and_then(|r| r.as_str());
                                            if let Some(reason) = finish_reason {
                                                let stop = if saw_function_calls {
                                                    StopReason::ToolUse
                                                } else {
                                                    match reason {
                                                        "STOP" => StopReason::EndTurn,
                                                        "MAX_TOKENS" => StopReason::MaxTokens,
                                                        "SAFETY" => StopReason::EndTurn,
                                                        _ => StopReason::EndTurn,
                                                    }
                                                };
                                                let _ = tx
                                                    .send(StreamEvent::MessageDelta {
                                                        stop_reason: Some(stop),
                                                        usage: Some(normalize_usage_metadata(
                                                            total_input_tokens,
                                                            total_output_tokens,
                                                            cached_content_tokens,
                                                        )),
                                                    })
                                                    .await;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(StreamEvent::Error {
                                message: e.to_string(),
                            })
                            .await;
                        return;
                    }
                }
            }

            let _ = tx.send(StreamEvent::MessageStop).await;
        });

        Ok(CompletionStream::new(rx))
    }
}

#[cfg(test)]
mod usage_tests {
    use super::normalize_usage_metadata;

    #[test]
    fn cached_content_is_split_from_total_prompt() {
        let usage = normalize_usage_metadata(100_000, 2_000, 75_000);
        assert_eq!(usage.input_tokens, 25_000);
        assert_eq!(usage.cache_read_input_tokens, 75_000);
        assert_eq!(usage.output_tokens, 2_000);
        assert_eq!(usage.prompt_tokens(), 100_000);
    }

    #[test]
    fn absent_cache_keeps_the_prompt_uncached() {
        let usage = normalize_usage_metadata(50_000, 1_000, 0);
        assert_eq!(usage.input_tokens, 50_000);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn malformed_cache_count_is_clamped_to_prompt() {
        let usage = normalize_usage_metadata(10, 1, 100);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 10);
        assert_eq!(usage.prompt_tokens(), 10);
    }
}

// ─── Multimodal helpers ──────────────────────────────────────────────────────

/// Build a Gemini `parts` entry for inline or remote media. Gemini accepts
/// images, video, audio, and PDFs the same way: `inlineData` for base64 bytes,
/// or `fileData` with a `fileUri` for remote/Files-API URLs.
fn gemini_media_part(
    mime: Option<&str>,
    data: Option<&str>,
    url: Option<&str>,
) -> Option<serde_json::Value> {
    let mime = mime.unwrap_or("application/octet-stream");
    if let Some(data) = data {
        return Some(serde_json::json!({
            "inlineData": { "mimeType": mime, "data": data },
        }));
    }
    if let Some(url) = url {
        return Some(serde_json::json!({
            "fileData": { "mimeType": mime, "fileUri": url },
        }));
    }
    None
}

// ─── Builder ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GeminiBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
}

impl GeminiBuilder {
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

    pub fn build(self) -> Result<Gemini> {
        let api_key = if let Some(key) = self.api_key {
            key
        } else {
            return Err(CerseiError::Auth(
                "No API key provided. Set GOOGLE_API_KEY or GEMINI_API_KEY or use .api_key()"
                    .into(),
            ));
        };

        Ok(Gemini {
            api_key,
            base_url: self.base_url.unwrap_or_else(|| GEMINI_API_BASE.to_string()),
            default_model: self
                .model
                .unwrap_or_else(|| "gemini-3.1-pro-preview".to_string()),
            client: reqwest::Client::new(),
        })
    }
}

// ─── B1, live ────────────────────────────────────────────────────────────────
//
// The offline suite proves `complete()` sends `adapt_tools` output
// (`tests/tool_body_shapes.rs`); these two prove the real API still agrees
// with the Exp 1/3 measurements that output is built on. Offline suites have
// already shipped one bug by asserting documented-but-wrong API behavior —
// this is the drift sentinel. Requires a key, so `#[ignore]`:
//
//   GEMINI_API_KEY=... cargo test -p cersei-provider --lib live_ \
//       -- --ignored --nocapture
//
// Override the model with CERSEI_LIVE_GEMINI_MODEL. The default is the model
// Exp 1/3 were measured against.
#[cfg(test)]
mod live_dialect_tests {
    use super::*;
    use crate::adapt::{adapt_tools, SchemaDialect};

    fn live_model() -> String {
        std::env::var("CERSEI_LIVE_GEMINI_MODEL")
            .unwrap_or_else(|_| "gemini-flash-lite-latest".to_string())
    }

    /// The schemars-0.8 shape Exp 1 measured being rejected: all four of the
    /// keys Gemini 400s on.
    fn schemars_like_tool() -> ToolDefinition {
        ToolDefinition {
            name: "Read".to_string(),
            description: "Reads a file".to_string(),
            input_schema: serde_json::json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file_path": { "type": "string" },
                    "range": { "$ref": "#/definitions/Range" },
                },
                "required": ["file_path"],
                "definitions": {
                    "Range": {
                        "type": "object",
                        "properties": { "start": { "type": "integer" } },
                    }
                }
            }),
        }
    }

    /// POST one non-streaming `generateContent` request carrying `decl` as
    /// the sole function declaration; return (status, response text). `None`
    /// (skip, not failure) when no key is present, matching the other live
    /// tests in this workspace.
    async fn post_live(decl: &serde_json::Value) -> Option<(u16, String)> {
        let key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
        {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("GOOGLE_API_KEY/GEMINI_API_KEY not set — skipping.");
                return None;
            }
        };
        let body = serde_json::json!({
            "contents": [{ "role": "user", "parts": [{ "text": "Read /tmp/x with the Read tool." }] }],
            "tools": [{ "functionDeclarations": [decl] }],
            "generationConfig": { "maxOutputTokens": 64 },
        });
        let resp = reqwest::Client::new()
            .post(format!(
                "{}/models/{}:generateContent",
                GEMINI_API_BASE,
                live_model()
            ))
            .header("x-goog-api-key", key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .expect("request to the Gemini API failed to send");
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Some((status, text))
    }

    /// Negative half: the RAW schemars shape must still be rejected. If this
    /// starts returning 2xx, Gemini has widened its dialect and
    /// `GeminiSubset` is stripping keys it no longer needs to.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and GEMINI_API_KEY set"]
    async fn live_raw_schemars_schema_is_rejected_by_the_real_api() {
        let t = schemars_like_tool();
        let raw_decl = serde_json::json!({
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        });
        let Some((status, text)) = post_live(&raw_decl).await else {
            return;
        };
        eprintln!("raw schemars schema → HTTP {status}");
        assert_eq!(
            status, 400,
            "Exp 1 measured INVALID_ARGUMENT for this shape; if Gemini now \
             accepts it, re-run the Exp 3 probes and revisit GeminiSubset: {text}"
        );
        assert!(
            text.contains("Unknown name") || text.contains("INVALID_ARGUMENT"),
            "rejected, but not for the measured reason: {text}"
        );
    }

    /// Positive half: what `GeminiSubset` builds from that same tool is
    /// accepted. If this 400s, the seam is emitting a shape the API does not
    /// take and B1 is not fixed.
    #[tokio::test]
    #[ignore = "live API test; run with --ignored and GEMINI_API_KEY set"]
    async fn live_adapted_schema_is_accepted_by_the_real_api() {
        let adapted = adapt_tools(&[schemars_like_tool()], SchemaDialect::GeminiSubset)
            .pop()
            .expect("one tool in, one out");
        let Some((status, text)) = post_live(&adapted).await else {
            return;
        };
        eprintln!("adapted schema → HTTP {status}");
        assert!(
            (200..300).contains(&status),
            "the declaration GeminiSubset builds was rejected with {status}: {text}"
        );
    }
}

#[cfg(test)]
mod multimodal_tests {
    use super::*;

    #[test]
    fn inline_data_for_base64() {
        let part = gemini_media_part(Some("video/mp4"), Some("QUJD"), None).unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "video/mp4");
        assert_eq!(part["inlineData"]["data"], "QUJD");
    }

    #[test]
    fn file_data_for_url() {
        let part = gemini_media_part(Some("image/png"), None, Some("gs://b/x.png")).unwrap();
        assert_eq!(part["fileData"]["mimeType"], "image/png");
        assert_eq!(part["fileData"]["fileUri"], "gs://b/x.png");
    }

    #[test]
    fn empty_source_yields_nothing() {
        assert!(gemini_media_part(Some("image/png"), None, None).is_none());
    }
}
