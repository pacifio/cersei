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
        Self {
            auth,
            base_url: OPENAI_API_BASE.to_string(),
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
                            if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
                                api_messages.push(serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": tool_use_id,
                                    "content": content,
                                }));
                            }
                        }
                        // Also include any text blocks as a user message
                        let text: String = blocks.iter().filter_map(|b| {
                            if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }
                        }).collect::<Vec<_>>().join("\n");
                        if !text.is_empty() {
                            api_messages.push(serde_json::json!({
                                "role": "user",
                                "content": text,
                            }));
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
                        let tool_uses: Vec<&ContentBlock> = blocks.iter().filter(|b| matches!(b, ContentBlock::ToolUse { .. })).collect();
                        if !tool_uses.is_empty() {
                            let tool_calls: Vec<serde_json::Value> = tool_uses.iter().map(|b| {
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
                            }).collect();

                            let text_content: String = blocks.iter().filter_map(|b| {
                                if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }
                            }).collect::<Vec<_>>().join("");

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
        let use_new_param = model.starts_with("gpt-5")
            || model.starts_with("o1")
            || model.starts_with("o3");

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

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        let url = format!("{}/chat/completions", self.base_url);
        let auth_header = match &self.auth {
            Auth::ApiKey(key) | Auth::Bearer(key) => format!("Bearer {}", key),
            Auth::OAuth { token, .. } => format!("Bearer {}", token.access_token),
            Auth::Custom(_) => String::new(),
        };

        let (tx, rx) = mpsc::channel(256);

        let req = self
            .client
            .post(&url)
            .header("authorization", &auth_header)
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
                    let mut text_started = false;
                    // Track tool calls being assembled across chunks
                    // OpenAI sends: tool_calls[i].id, tool_calls[i].function.name (first chunk)
                    //               tool_calls[i].function.arguments (subsequent chunks, accumulated)
                    let mut tool_calls: std::collections::HashMap<usize, (String, String, String)> = std::collections::HashMap::new(); // index -> (id, name, args_json)
                    let mut has_tool_calls = false;

                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(bytes) => {
                                buffer.push_str(&String::from_utf8_lossy(&bytes));
                                while let Some(pos) = buffer.find("\n") {
                                    let line = buffer[..pos].to_string();
                                    buffer = buffer[pos + 1..].to_string();

                                    if let Some(data) = line.strip_prefix("data: ") {
                                        let data = data.trim();
                                        if data == "[DONE]" {
                                            // Emit accumulated tool calls
                                            for (idx, (id, name, args)) in &tool_calls {
                                                let input: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
                                                let _ = tx.send(StreamEvent::ContentBlockStart {
                                                    index: *idx + 1,
                                                    block_type: "tool_use".into(),
                                                    id: Some(id.clone()),
                                                    name: Some(name.clone()),
                                                }).await;
                                                // Send full args as InputJsonDelta
                                                let _ = tx.send(StreamEvent::InputJsonDelta {
                                                    index: *idx + 1,
                                                    partial_json: args.clone(),
                                                }).await;
                                                let _ = tx.send(StreamEvent::ContentBlockStop { index: *idx + 1 }).await;
                                            }

                                            if text_started {
                                                let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;
                                            }

                                            let stop = if has_tool_calls {
                                                StopReason::ToolUse
                                            } else {
                                                StopReason::EndTurn
                                            };

                                            // Extract usage if available
                                            let _ = tx.send(StreamEvent::MessageDelta {
                                                stop_reason: Some(stop),
                                                usage: None,
                                            }).await;
                                            let _ = tx.send(StreamEvent::MessageStop).await;
                                            return;
                                        }

                                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                            let delta = &json["choices"][0]["delta"];
                                            let finish_reason = json["choices"][0]["finish_reason"].as_str();

                                            // Text content
                                            if let Some(text) = delta["content"].as_str() {
                                                if !text_started {
                                                    text_started = true;
                                                    let _ = tx.send(StreamEvent::ContentBlockStart {
                                                        index: 0,
                                                        block_type: "text".into(),
                                                        id: None,
                                                        name: None,
                                                    }).await;
                                                }
                                                let _ = tx.send(StreamEvent::TextDelta {
                                                    index: 0,
                                                    text: text.to_string(),
                                                }).await;
                                            }

                                            // Tool calls (accumulated across chunks)
                                            if let Some(tc_array) = delta["tool_calls"].as_array() {
                                                has_tool_calls = true;
                                                for tc in tc_array {
                                                    let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                                                    let entry = tool_calls.entry(idx).or_insert_with(|| {
                                                        (String::new(), String::new(), String::new())
                                                    });

                                                    // First chunk has id and function.name
                                                    if let Some(id) = tc["id"].as_str() {
                                                        entry.0 = id.to_string();
                                                    }
                                                    if let Some(name) = tc["function"]["name"].as_str() {
                                                        entry.1 = name.to_string();
                                                    }
                                                    // Arguments accumulate across chunks
                                                    if let Some(args) = tc["function"]["arguments"].as_str() {
                                                        entry.2.push_str(args);
                                                    }
                                                }
                                            }

                                            // Usage from the final chunk
                                            if let Some(usage) = json["usage"].as_object() {
                                                let input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                                let output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                                let _ = tx.send(StreamEvent::MessageDelta {
                                                    stop_reason: finish_reason.and_then(|r| match r {
                                                        "stop" => Some(StopReason::EndTurn),
                                                        "tool_calls" => Some(StopReason::ToolUse),
                                                        "length" => Some(StopReason::MaxTokens),
                                                        _ => None,
                                                    }),
                                                    usage: Some(Usage {
                                                        input_tokens,
                                                        output_tokens,
                                                        ..Default::default()
                                                    }),
                                                }).await;
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
