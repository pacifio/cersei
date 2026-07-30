//! Stream accumulator: collects SSE stream events into a complete response.

use cersei_types::*;
use std::collections::HashMap;

/// Accumulates streaming events into content blocks.
pub struct StreamAccumulator {
    content_blocks: Vec<ContentBlock>,
    partial_text: HashMap<usize, String>,
    partial_json: HashMap<usize, String>,
    partial_thinking: HashMap<usize, String>,
    block_types: HashMap<usize, String>,
    tool_use_ids: HashMap<usize, String>,
    tool_use_names: HashMap<usize, String>,
    stop_reason: Option<StopReason>,
    usage: Usage,
    model: Option<String>,
    message_id: Option<String>,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            partial_text: HashMap::new(),
            partial_json: HashMap::new(),
            partial_thinking: HashMap::new(),
            block_types: HashMap::new(),
            tool_use_ids: HashMap::new(),
            tool_use_names: HashMap::new(),
            stop_reason: None,
            usage: Usage::default(),
            model: None,
            message_id: None,
        }
    }

    pub fn process_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::MessageStart { id, model } => {
                self.message_id = Some(id);
                self.model = Some(model);
            }
            StreamEvent::ContentBlockStart {
                index,
                block_type,
                id,
                name,
            } => {
                self.block_types.insert(index, block_type);
                if let Some(id) = id {
                    self.tool_use_ids.insert(index, id);
                }
                if let Some(name) = name {
                    self.tool_use_names.insert(index, name);
                }
            }
            StreamEvent::TextDelta { index, text } => {
                self.partial_text.entry(index).or_default().push_str(&text);
            }
            StreamEvent::InputJsonDelta {
                index,
                partial_json,
            } => {
                self.partial_json
                    .entry(index)
                    .or_default()
                    .push_str(&partial_json);
            }
            StreamEvent::ThinkingDelta { index, thinking } => {
                self.partial_thinking
                    .entry(index)
                    .or_default()
                    .push_str(&thinking);
            }
            StreamEvent::ContentBlockStop { index } => {
                let block_type = self.block_types.get(&index).cloned().unwrap_or_default();
                let block = match block_type.as_str() {
                    "text" => ContentBlock::Text {
                        text: self.partial_text.remove(&index).unwrap_or_default(),
                    },
                    "tool_use" => {
                        let json_str = self.partial_json.remove(&index).unwrap_or_default();
                        let input =
                            serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);
                        ContentBlock::ToolUse {
                            id: self.tool_use_ids.remove(&index).unwrap_or_default(),
                            name: self.tool_use_names.remove(&index).unwrap_or_default(),
                            input,
                        }
                    }
                    "thinking" => ContentBlock::Thinking {
                        thinking: self.partial_thinking.remove(&index).unwrap_or_default(),
                        signature: String::new(),
                    },
                    _ => ContentBlock::Text {
                        text: self.partial_text.remove(&index).unwrap_or_default(),
                    },
                };
                // Ensure we have enough slots
                while self.content_blocks.len() <= index {
                    self.content_blocks.push(ContentBlock::Text {
                        text: String::new(),
                    });
                }
                self.content_blocks[index] = block;
            }
            StreamEvent::MessageDelta { stop_reason, usage } => {
                if let Some(sr) = stop_reason {
                    self.stop_reason = Some(sr);
                }
                if let Some(u) = usage {
                    self.usage.merge(&u);
                }
            }
            StreamEvent::MessageStop => {}
            StreamEvent::Ping => {}
            StreamEvent::Error { .. } => {}
        }
    }

    pub fn into_response(self) -> Result<super::CompletionResponse> {
        let message = Message {
            role: Role::Assistant,
            content: if self.content_blocks.is_empty() {
                MessageContent::Text(String::new())
            } else {
                MessageContent::Blocks(self.content_blocks)
            },
            id: self.message_id,
            metadata: Some(MessageMetadata {
                model: self.model,
                usage: Some(self.usage.clone()),
                stop_reason: self.stop_reason.clone(),
                provider_data: serde_json::Value::Null,
            }),
        };

        Ok(super::CompletionResponse {
            message,
            usage: self.usage,
            stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
        })
    }

    /// Get accumulated text so far (for streaming display).
    pub fn current_text(&self) -> String {
        self.partial_text.values().cloned().collect()
    }
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}
