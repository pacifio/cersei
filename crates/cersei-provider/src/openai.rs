//! OpenAI-compatible provider (works with OpenAI, Azure, Ollama, etc.)

use crate::*;
use cersei_types::*;
use futures::StreamExt;
use tokio::sync::mpsc;

const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

pub struct OpenAi {
    auth: Auth,
    base_url: String,
    default_model: String,
    client: reqwest::Client,
}

impl OpenAi {
    pub fn new(auth: Auth) -> Self {
        let base_url = std::env::var("OPENAI_BASE_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| OPENAI_API_BASE.to_string());
        Self {
            auth,
            base_url,
            default_model: "gpt-4o".to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| CerseiError::Auth("OPENAI_API_KEY not set".into()))?;
        Ok(Self::new(Auth::ApiKey(key)))
    }

    pub fn builder() -> OpenAiBuilder {
        OpenAiBuilder::default()
    }
}

#[async_trait::async_trait]
impl Provider for OpenAi {
    fn name(&self) -> &str {
        "openai"
    }

    fn context_window(&self, model: &str) -> u64 {
        match model {
            m if m.contains("gpt-5") => 1_000_000,
            m if m.starts_with("o1") || m.starts_with("o3") => 200_000,
            m if m.contains("gpt-4o") => 128_000,
            m if m.contains("gpt-4-turbo") => 128_000,
            m if m.contains("gpt-4") => 8_192,
            m if m.contains("gpt-3.5") => 16_385,
            _ => 128_000,
        }
    }

    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_use: true,
            vision: true,
            thinking: false,
            system_prompt: true,
            caching: false,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        // Build OpenAI-format messages
        let mut api_messages: Vec<serde_json::Value> = Vec::new();

        if let Some(system) = &request.system {
            api_messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        for msg in &request.messages {
            match msg.role {
                Role::User => {
                    // Check if this is a tool result message
                    if let MessageContent::Blocks(blocks) = &msg.content {
                        for block in blocks {
                            // `is_error` is deliberately discarded: OpenAI's
                            // `role:"tool"` message has no error field on the
                            // wire. The failure signal still reaches the model
                            // because the runner appends its error note into
                            // the result content itself (see §2.1 of the
                            // tool-calling audit).
                            if let ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error: _,
                            } = block
                            {
                                api_messages.push(serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": tool_use_id,
                                    "content": content,
                                }));
                            }
                        }
                        // Collect text + multimodal (image/PDF) parts into a
                        // single user message. OpenAI takes content as an array
                        // of typed parts when any non-text media is present.
                        let mut parts: Vec<serde_json::Value> = Vec::new();
                        for block in blocks {
                            match block {
                                ContentBlock::Text { text } => {
                                    parts.push(serde_json::json!({
                                        "type": "text",
                                        "text": text,
                                    }));
                                }
                                ContentBlock::Image { source } => {
                                    if let Some(url) = openai_image_url(source) {
                                        parts.push(serde_json::json!({
                                            "type": "image_url",
                                            "image_url": { "url": url },
                                        }));
                                    }
                                }
                                ContentBlock::Document { source, .. } => {
                                    if let Some(part) = openai_file_part(source) {
                                        parts.push(part);
                                    }
                                }
                                _ => {}
                            }
                        }
                        match parts.as_slice() {
                            [] => {}
                            // A single text part collapses to a plain string for
                            // backward compatibility with text-only callers.
                            [only] if only["type"] == "text" => {
                                api_messages.push(serde_json::json!({
                                    "role": "user",
                                    "content": only["text"].clone(),
                                }));
                            }
                            _ => {
                                api_messages.push(serde_json::json!({
                                    "role": "user",
                                    "content": parts,
                                }));
                            }
                        }
                    } else {
                        api_messages.push(serde_json::json!({
                            "role": "user",
                            "content": msg.get_all_text(),
                        }));
                    }
                }
                Role::Assistant => {
                    // Check for tool_use blocks — serialize as tool_calls
                    if let MessageContent::Blocks(blocks) = &msg.content {
                        let tool_uses: Vec<&ContentBlock> = blocks
                            .iter()
                            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                            .collect();
                        if !tool_uses.is_empty() {
                            let tool_calls: Vec<serde_json::Value> = tool_uses
                                .iter()
                                .map(|b| {
                                    if let ContentBlock::ToolUse { id, name, input } = b {
                                        serde_json::json!({
                                            "id": id,
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": input.to_string(),
                                            }
                                        })
                                    } else {
                                        serde_json::json!({})
                                    }
                                })
                                .collect();

                            let text_content: String = blocks
                                .iter()
                                .filter_map(|b| {
                                    if let ContentBlock::Text { text } = b {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("");

                            let mut asst_msg = serde_json::json!({
                                "role": "assistant",
                                "tool_calls": tool_calls,
                            });
                            if !text_content.is_empty() {
                                asst_msg["content"] = serde_json::json!(text_content);
                            }
                            api_messages.push(asst_msg);
                        } else {
                            api_messages.push(serde_json::json!({
                                "role": "assistant",
                                "content": msg.get_all_text(),
                            }));
                        }
                    } else {
                        api_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": msg.get_all_text(),
                        }));
                    }
                }
                Role::System => {
                    api_messages.push(serde_json::json!({
                        "role": "system",
                        "content": msg.get_all_text(),
                    }));
                }
            }
        }

        // GPT-5+ and o-series use max_completion_tokens; older models use max_tokens
        let use_new_param =
            model.starts_with("gpt-5") || model.starts_with("o1") || model.starts_with("o3");

        let mut body = if use_new_param {
            serde_json::json!({
                "model": model,
                "messages": api_messages,
                "max_completion_tokens": request.max_tokens,
                "stream": true,
                "stream_options": { "include_usage": true },
            })
        } else {
            serde_json::json!({
                "model": model,
                "messages": api_messages,
                "max_tokens": request.max_tokens,
                "stream": true,
                "stream_options": { "include_usage": true },
            })
        };

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Reasoning effort: provider-agnostic `reasoning_effort` option
        // ("minimal"/"low"/"medium"/"high"), mapped onto the OpenAI request body.
        // Only the o-series / gpt-5 reasoning models accept it.
        if let Some(effort) = reasoning_effort_for(&model, &request.options) {
            body["reasoning_effort"] = serde_json::json!(effort);
        }

        if !request.tools.is_empty() {
            // B1: schemas cross the provider boundary only through
            // `adapt_tools`. Loose until B2's quirks opt a model into strict.
            let tools =
                crate::adapt::adapt_tools(&request.tools, crate::adapt::SchemaDialect::OpenAiLoose);
            body["tools"] = serde_json::Value::Array(tools);
        }

        let url = format!("{}/chat/completions", self.base_url);
        let auth_header = match &self.auth {
            Auth::ApiKey(key) | Auth::Bearer(key) => format!("Bearer {}", key),
            Auth::OAuth { token, .. } => format!("Bearer {}", token.access_token),
            Auth::Custom(_) => String::new(),
        };

        let req = self
            .client
            .post(&url)
            .header("authorization", &auth_header)
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
                })
                .await;
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut text_started = false;
            // Track tool calls being assembled across chunks
            // OpenAI sends: tool_calls[i].id, tool_calls[i].function.name (first chunk)
            //               tool_calls[i].function.arguments (subsequent chunks, accumulated)
            // Ordered so the post-loop flush emits calls in ascending slot
            // order (a HashMap made live event order nondeterministic).
            let mut tool_calls: std::collections::BTreeMap<usize, (String, String, String)> =
                std::collections::BTreeMap::new(); // slot -> (id, name, args_json)
            // F-A2: servers that omit `tool_calls[].index` (llama.cpp, some
            // Ollama builds, LiteLLM proxies) get synthetic slots correlated
            // by call id, instead of every parallel call collapsing onto slot
            // 0 and its argument bodies concatenating into invalid JSON.
            let mut slot_for_id: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut last_slot: Option<usize> = None;
            // F-03: tool calls are flushed exactly once, after the read loop.
            // `[DONE]` now only records that the stream terminated cleanly.
            let mut saw_done = false;
            let mut final_stop: Option<StopReason> = None;

            'read: while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(pos) = buffer.find("\n") {
                            let line = buffer[..pos].to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if let Some(data) = line.strip_prefix("data: ") {
                                let data = data.trim();
                                if data == "[DONE]" {
                                    // F-03: do not flush here. Record the
                                    // clean termination and fall out to the
                                    // single finalize block after the read
                                    // loop, which also runs on plain EOF.
                                    saw_done = true;
                                    break 'read;
                                }

                                if let Ok(json) =
                                    serde_json::from_str::<serde_json::Value>(data)
                                {
                                    let delta = &json["choices"][0]["delta"];
                                    let finish_reason =
                                        json["choices"][0]["finish_reason"].as_str();
                                    // Capture the terminal reason wherever it
                                    // appears. The mapping further down only
                                    // runs when the same chunk also carries
                                    // `usage`, and the finalize MessageDelta
                                    // would overwrite it regardless.
                                    match finish_reason {
                                        Some("stop") => {
                                            final_stop = Some(StopReason::EndTurn)
                                        }
                                        Some("tool_calls") => {
                                            final_stop = Some(StopReason::ToolUse)
                                        }
                                        Some("length") => {
                                            final_stop = Some(StopReason::MaxTokens)
                                        }
                                        _ => {}
                                    }

                                    // Text content
                                    if let Some(text) = delta["content"].as_str() {
                                        if !text_started {
                                            text_started = true;
                                            let _ = tx
                                                .send(StreamEvent::ContentBlockStart {
                                                    index: 0,
                                                    block_type: "text".into(),
                                                    id: None,
                                                    name: None,
                                                })
                                                .await;
                                        }
                                        let _ = tx
                                            .send(StreamEvent::TextDelta {
                                                index: 0,
                                                text: text.to_string(),
                                            })
                                            .await;
                                    }

                                    // Tool calls (accumulated across chunks)
                                    if let Some(tc_array) = delta["tool_calls"].as_array() {
                                        for tc in tc_array {
                                            let tc_id =
                                                tc["id"].as_str().filter(|s| !s.is_empty());
                                            // F-A2: only an explicit `index` is
                                            // trusted. Without one, correlate by
                                            // call id so parallel calls land in
                                            // distinct slots; an id-less delta is
                                            // a continuation of the slot most
                                            // recently touched.
                                            let idx = match tc["index"].as_u64() {
                                                Some(i) => i as usize,
                                                None => match tc_id
                                                    .and_then(|id| {
                                                        slot_for_id.get(id).copied()
                                                    }) {
                                                    Some(slot) => slot,
                                                    None if tc_id.is_some() => tool_calls
                                                        .keys()
                                                        .next_back()
                                                        .map(|k| k + 1)
                                                        .unwrap_or(0),
                                                    None => last_slot.unwrap_or(0),
                                                },
                                            };
                                            if let Some(id) = tc_id {
                                                slot_for_id.insert(id.to_string(), idx);
                                            }
                                            last_slot = Some(idx);
                                            let entry = tool_calls
                                                .entry(idx)
                                                .or_insert_with(|| {
                                                    (
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                    )
                                                });

                                            // First chunk has id and function.name.
                                            // F-A3: never let an empty-string field
                                            // from a later delta clobber a good one.
                                            if let Some(id) = tc_id {
                                                entry.0 = id.to_string();
                                            }
                                            if let Some(name) = tc["function"]["name"]
                                                .as_str()
                                                .filter(|s| !s.is_empty())
                                            {
                                                entry.1 = name.to_string();
                                            }
                                            // Arguments accumulate across chunks
                                            if let Some(args) =
                                                tc["function"]["arguments"].as_str()
                                            {
                                                entry.2.push_str(args);
                                            }
                                        }
                                    }

                                    // Usage from the final chunk
                                    if let Some(usage) = json["usage"].as_object() {
                                        let input_tokens = usage
                                            .get("prompt_tokens")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0);
                                        let output_tokens = usage
                                            .get("completion_tokens")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0);
                                        let _ = tx
                                            .send(StreamEvent::MessageDelta {
                                                stop_reason: finish_reason.and_then(|r| {
                                                    match r {
                                                        "stop" => Some(StopReason::EndTurn),
                                                        "tool_calls" => {
                                                            Some(StopReason::ToolUse)
                                                        }
                                                        "length" => {
                                                            Some(StopReason::MaxTokens)
                                                        }
                                                        _ => None,
                                                    }
                                                }),
                                                usage: Some(Usage {
                                                    input_tokens,
                                                    output_tokens,
                                                    ..Default::default()
                                                }),
                                            })
                                            .await;
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

            // ── Finalize (F-03) ──────────────────────────────────────
            // Reached both on `[DONE]` (via `break 'read`) and on plain
            // EOF. This is the ONLY place tool calls are emitted, so a
            // cleanly terminated stream cannot double-emit.
            let mut emitted = 0usize;
            // Subset of `emitted` the runner can actually *dispatch*:
            // routable id/name AND arguments stream.rs will hand over as
            // real input instead of stamping `__parse_error` on. Truncated
            // arguments still get emitted (the dispatch layer echoes the
            // parse error plus the schema back to the model, F-05), but a
            // call nobody can run is not evidence the turn produced work,
            // so it must not override a truncation stop reason.
            let mut executable = 0usize;
            let mut rejected: Vec<String> = Vec::new();
            for (idx, (id, name, args)) in &tool_calls {
                // F-A3: never emit a call the dispatch layer cannot route.
                // An empty name produces "Unknown tool: "; an empty id is
                // echoed back as "tool_call_id": "" on the next request and
                // rejected with a 400, wedging the conversation permanently.
                if id.is_empty() || name.is_empty() {
                    rejected.push(format!(
                        "slot {}: id={:?} name={:?} arguments={:?}",
                        idx, id, name, args
                    ));
                    continue;
                }
                let _ = tx
                    .send(StreamEvent::ContentBlockStart {
                        index: *idx + 1,
                        block_type: "tool_use".into(),
                        id: Some(id.clone()),
                        name: Some(name.clone()),
                    })
                    .await;
                let _ = tx
                    .send(StreamEvent::InputJsonDelta {
                        index: *idx + 1,
                        partial_json: args.clone(),
                    })
                    .await;
                let _ = tx
                    .send(StreamEvent::ContentBlockStop { index: *idx + 1 })
                    .await;
                emitted += 1;
                // Mirror stream.rs's ContentBlockStop parse exactly: empty
                // arguments are a no-argument call (`{}`); anything that
                // deserializes is usable; only a parse failure is not.
                if args.trim().is_empty()
                    || serde_json::from_str::<serde_json::Value>(args).is_ok()
                {
                    executable += 1;
                }
            }

            // P1-HIGH: an unusable call must not take its valid siblings
            // down with it. When something usable survived, report the loss
            // in-band on this same assistant message rather than raising a
            // stream-level Error — stream.rs short-circuits `into_response`
            // on the first error and never looks at `content_blocks`, so the
            // good calls would be silently destroyed and the turn aborted.
            // A text block reaches the model on the next request (assistant
            // `content` alongside `tool_calls`), so it can re-issue whatever
            // was dropped.
            if !rejected.is_empty() && emitted > 0 {
                tracing::warn!(
                    rejected = rejected.len(),
                    emitted,
                    "provider emitted unusable tool call(s); keeping the valid ones"
                );
                let note = format!(
                    "{}[cersei] dropped {} unusable tool call(s) (empty id or name): {}. \
                     {} valid call(s) were kept; re-issue the dropped one(s) if you \
                     still need them.",
                    if text_started { "\n\n" } else { "" },
                    rejected.len(),
                    rejected.join("; "),
                    emitted
                );
                if !text_started {
                    text_started = true;
                    let _ = tx
                        .send(StreamEvent::ContentBlockStart {
                            index: 0,
                            block_type: "text".into(),
                            id: None,
                            name: None,
                        })
                        .await;
                }
                let _ = tx
                    .send(StreamEvent::TextDelta {
                        index: 0,
                        text: note,
                    })
                    .await;
            }

            if text_started {
                let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;
            }

            let stop = match final_stop {
                // P1-BLOCKER: `finish_reason` describes how generation
                // *ended*, not what it produced. Once a dispatchable call is
                // on the wire it must be run, and ToolUse is the only stop
                // reason that makes the runner dispatch it. Any other value
                // drops the calls while the assistant message is still
                // serialized WITH `tool_calls` (see the Role::Assistant arm
                // above), so the next request carries a tool_call that no
                // `role: "tool"` message answers -> provider 400 ->
                // CerseiError::Provider, which is not retryable -> the
                // conversation is wedged for good. "length" is the dangerous
                // one: hitting the cap on the token *after* a complete call
                // still leaves that call fully executable.
                _ if executable > 0 => StopReason::ToolUse,
                // Some servers report "stop" even when they emitted tool
                // calls; the calls are the ground truth.
                Some(StopReason::EndTurn) if emitted > 0 => StopReason::ToolUse,
                // Nothing dispatchable came out, so the provider's own
                // reason stands (a truncated call really is MaxTokens).
                Some(sr) => sr,
                None if emitted > 0 => StopReason::ToolUse,
                None => StopReason::EndTurn,
            };
            let _ = tx
                .send(StreamEvent::MessageDelta {
                    stop_reason: Some(stop),
                    usage: None,
                })
                .await;

            // Surface what the stream got wrong instead of laundering it —
            // but only kill the turn when the rejection left nothing to run.
            // The surviving-siblings case was already reported in-band above.
            if !rejected.is_empty() && emitted == 0 {
                let _ = tx
                    .send(StreamEvent::Error {
                        message: format!(
                            "provider emitted {} unusable tool call(s) \
                             (empty id or name): {}",
                            rejected.len(),
                            rejected.join("; ")
                        ),
                    })
                    .await;
            } else if !saw_done && emitted == 0 && !text_started {
                // A stream that was cut short AND yielded nothing. Without
                // this the accumulator reports a confident, empty EndTurn.
                let _ = tx
                    .send(StreamEvent::Error {
                        message: "stream ended without [DONE] and produced no content"
                            .into(),
                    })
                    .await;
            }

            let _ = tx.send(StreamEvent::MessageStop).await;
        });

        Ok(CompletionStream::new(rx))
    }
}

// ─── Multimodal helpers ──────────────────────────────────────────────────────

/// Convert an [`ImageSource`] to the `image_url.url` string OpenAI expects.
/// Base64 sources become `data:` URLs; remote URL sources pass through. Returns
/// `None` for non-image media (e.g. video), which the Chat Completions API can't
/// accept, so it's dropped rather than rejected by the server.
fn openai_image_url(source: &ImageSource) -> Option<String> {
    if let Some(mt) = &source.media_type {
        if !mt.starts_with("image/") {
            return None;
        }
    }
    if let Some(url) = &source.url {
        return Some(url.clone());
    }
    let data = source.data.as_ref()?;
    let mt = source.media_type.as_deref().unwrap_or("image/png");
    Some(format!("data:{mt};base64,{data}"))
}

/// Convert a [`DocumentSource`] to an OpenAI `file` content part. Only base64
/// data is supported (sent as a `file_data` data URL); URL-only documents are
/// dropped since Chat Completions has no remote-file fetch.
fn openai_file_part(source: &DocumentSource) -> Option<serde_json::Value> {
    let data = source.data.as_ref()?;
    let mt = source.media_type.as_deref().unwrap_or("application/pdf");
    Some(serde_json::json!({
        "type": "file",
        "file": { "file_data": format!("data:{mt};base64,{data}") },
    }))
}

/// Resolve the OpenAI `reasoning_effort` request field from the provider-agnostic
/// `reasoning_effort` option, gated to models that accept it (o-series / gpt-5).
/// Returns `None` (omit the field) for non-reasoning models or when unset.
fn reasoning_effort_for(model: &str, options: &ProviderOptions) -> Option<String> {
    let reasoning_model =
        model.starts_with("gpt-5") || model.starts_with("o1") || model.starts_with("o3");
    if !reasoning_model {
        return None;
    }
    options.get::<String>("reasoning_effort")
}

// ─── Builder ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct OpenAiBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
}

impl OpenAiBuilder {
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

    pub fn build(self) -> Result<OpenAi> {
        let auth = if let Some(key) = self.api_key {
            Auth::ApiKey(key)
        } else {
            return Err(CerseiError::Auth(
                "No API key provided. Set OPENAI_API_KEY or use .api_key()".into(),
            ));
        };

        Ok(OpenAi {
            auth,
            base_url: self.base_url.unwrap_or_else(|| OPENAI_API_BASE.to_string()),
            default_model: self.model.unwrap_or_else(|| "gpt-4o".to_string()),
            client: reqwest::Client::new(),
        })
    }
}

#[cfg(test)]
mod multimodal_tests {
    use super::*;

    #[test]
    fn base64_image_becomes_data_url() {
        let block = ContentBlock::image_base64("image/png", "QUJD");
        let ContentBlock::Image { source } = block else {
            panic!("expected image");
        };
        assert_eq!(
            openai_image_url(&source).as_deref(),
            Some("data:image/png;base64,QUJD")
        );
    }

    #[test]
    fn remote_image_url_passes_through() {
        let block = ContentBlock::image_url("https://x/y.jpg");
        let ContentBlock::Image { source } = block else {
            panic!("expected image");
        };
        assert_eq!(openai_image_url(&source).as_deref(), Some("https://x/y.jpg"));
    }

    #[test]
    fn video_is_dropped_for_openai() {
        let block = ContentBlock::image_bytes("video/mp4", b"data");
        let ContentBlock::Image { source } = block else {
            panic!("expected image");
        };
        assert_eq!(openai_image_url(&source), None);
    }

    #[test]
    fn pdf_becomes_file_part() {
        let block = ContentBlock::document_base64("application/pdf", "UERG");
        let ContentBlock::Document { source, .. } = block else {
            panic!("expected document");
        };
        let part = openai_file_part(&source).unwrap();
        assert_eq!(part["type"], "file");
        assert_eq!(part["file"]["file_data"], "data:application/pdf;base64,UERG");
    }

    #[test]
    fn reasoning_effort_only_on_reasoning_models_when_set() {
        let mut opts = ProviderOptions::default();
        opts.set("reasoning_effort", "high");

        // Reasoning models map the option through...
        assert_eq!(reasoning_effort_for("gpt-5.3", &opts).as_deref(), Some("high"));
        assert_eq!(reasoning_effort_for("o3-mini", &opts).as_deref(), Some("high"));
        // ...non-reasoning models omit it...
        assert_eq!(reasoning_effort_for("gpt-4o", &opts), None);
        // ...and an unset option omits it even on reasoning models.
        assert_eq!(reasoning_effort_for("gpt-5.3", &ProviderOptions::default()), None);
    }
}
