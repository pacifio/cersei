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

// ─── Gemini provider ────────────────────────────────────────────────────────

pub struct Gemini {
    api_key: String,
    base_url: String,
    default_model: String,
    client: reqwest::Client,
}

impl Gemini {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: GEMINI_API_BASE.to_string(),
            default_model: "gemini-2.0-flash".to_string(),
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
            m if m.contains("gemini-2.0") => 1_000_000,
            m if m.contains("gemini-1.5-pro") => 2_000_000,
            m if m.contains("gemini-1.5-flash") => 1_000_000,
            _ => 1_000_000,
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
                                ContentBlock::ToolResult { tool_use_id, content, .. } => {
                                    // Gemini represents tool results as functionResponse parts
                                    let response_data = serde_json::json!({
                                        "content": content,
                                    });
                                    parts.push(serde_json::json!({
                                        "functionResponse": {
                                            "name": tool_use_id,
                                            "response": response_data,
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
                                ContentBlock::ToolUse { id: _, name, input } => {
                                    parts.push(serde_json::json!({
                                        "functionCall": {
                                            "name": name,
                                            "args": input,
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

        // System instruction (Gemini's equivalent of system prompt)
        if let Some(system) = &request.system {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{ "text": system }],
            });
        }

        if let Some(temp) = request.temperature {
            body["generationConfig"]["temperature"] = serde_json::json!(temp);
        }

        if !request.stop_sequences.is_empty() {
            body["generationConfig"]["stopSequences"] = serde_json::json!(request.stop_sequences);
        }

        // Tool declarations
        if !request.tools.is_empty() {
            let function_declarations: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    })
                })
                .collect();
            body["tools"] = serde_json::json!([{
                "functionDeclarations": function_declarations,
            }]);
        }

        // Safety settings: use least restrictive defaults to avoid unexpected blocks
        body["safetySettings"] = serde_json::json!([
            { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "BLOCK_ONLY_HIGH" },
        ]);

        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url, model, self.api_key
        );

        let (tx, rx) = mpsc::channel(256);

        let req = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .build()
            .map_err(CerseiError::Http)?;

        let client = self.client.clone();

        tokio::spawn(async move {
            match client.execute(req).await {
                Ok(response) => {
                    if !response.status().is_success() {
                        let status = response.status().as_u16();
                        let body = response.text().await.unwrap_or_default();
                        let _ = tx
                            .send(StreamEvent::Error {
                                message: format!("HTTP {}: {}", status, body),
                            })
                            .await;
                        return;
                    }

                    let _ = tx
                        .send(StreamEvent::MessageStart {
                            id: String::new(),
                            model: String::new(),
                        })
                        .await;

                    let mut stream = response.bytes_stream();
                    let mut buffer = String::new();
                    let mut block_index: usize = 0;
                    let mut total_input_tokens: u64 = 0;
                    let mut total_output_tokens: u64 = 0;

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
                                            }

                                            // Process candidates
                                            if let Some(candidates) = json.get("candidates").and_then(|c| c.as_array()) {
                                                for candidate in candidates {
                                                    if let Some(parts) = candidate
                                                        .get("content")
                                                        .and_then(|c| c.get("parts"))
                                                        .and_then(|p| p.as_array())
                                                    {
                                                        for part in parts {
                                                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
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
                                                                let name = fc
                                                                    .get("name")
                                                                    .and_then(|n| n.as_str())
                                                                    .unwrap_or("")
                                                                    .to_string();
                                                                let args = fc
                                                                    .get("args")
                                                                    .cloned()
                                                                    .unwrap_or(serde_json::Value::Object(Default::default()));
                                                                let tool_id = format!("gemini-tool-{}", block_index);

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
                                                                        partial_json: serde_json::to_string(&args)
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
                                                        let stop = match reason {
                                                            "STOP" => StopReason::EndTurn,
                                                            "MAX_TOKENS" => StopReason::MaxTokens,
                                                            "SAFETY" => StopReason::EndTurn,
                                                            _ => StopReason::EndTurn,
                                                        };
                                                        let _ = tx
                                                            .send(StreamEvent::MessageDelta {
                                                                stop_reason: Some(stop),
                                                                usage: Some(Usage {
                                                                    input_tokens: total_input_tokens,
                                                                    output_tokens: total_output_tokens,
                                                                    ..Default::default()
                                                                }),
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
                }
                Err(e) => {
                    let _ = tx
                        .send(StreamEvent::Error {
                            message: e.to_string(),
                        })
                        .await;
                }
            }
        });

        Ok(CompletionStream::new(rx))
    }
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
            default_model: self.model.unwrap_or_else(|| "gemini-2.0-flash".to_string()),
            client: reqwest::Client::new(),
        })
    }
}
