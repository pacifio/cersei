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
    /// First mid-stream provider error, if any. Recorded rather than discarded so
    /// `into_response` can fail loudly instead of returning a clean turn (F-03b).
    stream_error: Option<String>,
    /// True once a terminal event arrived: `MessageStop`, or a `MessageDelta`
    /// carrying a `stop_reason`. Distinguishes a turn the provider actually ended
    /// from a stream that was cut off mid-flight (F-03c).
    saw_terminal: bool,
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
            stream_error: None,
            saw_terminal: false,
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
                        let input = if json_str.trim().is_empty() {
                            // No input_json_delta events at all: this is a
                            // no-argument call, which is `{}`, never `null` (F-05).
                            serde_json::Value::Object(serde_json::Map::new())
                        } else {
                            match serde_json::from_str::<serde_json::Value>(&json_str) {
                                // A literal `null` arguments payload is also a
                                // no-argument call, not a type error.
                                Ok(serde_json::Value::Null) => {
                                    serde_json::Value::Object(serde_json::Map::new())
                                }
                                Ok(value) => value,
                                // Preserve BOTH the parse error and the raw text so
                                // the dispatch layer can echo them plus the tool's
                                // own schema back to the model (F-05).
                                Err(err) => serde_json::json!({
                                    "__parse_error": err.to_string(),
                                    "__raw": json_str,
                                }),
                            }
                        };
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
                    // The provider told us why the turn ended: that is a terminal
                    // signal even if `MessageStop` never arrives.
                    self.saw_terminal = true;
                    self.stop_reason = Some(sr);
                }
                if let Some(u) = usage {
                    self.usage.merge(&u);
                }
            }
            StreamEvent::MessageStop => {
                self.saw_terminal = true;
            }
            StreamEvent::Ping => {}
            StreamEvent::Error { message } => {
                // Do not swallow it (F-03b). Keep the first error: later ones on the
                // same stream are usually cascade noise.
                if self.stream_error.is_none() {
                    self.stream_error = Some(message);
                }
            }
        }
    }

    pub fn into_response(self) -> Result<super::CompletionResponse> {
        // A provider error mid-stream is a failed turn, not a partial success (F-03b).
        if let Some(message) = self.stream_error.clone() {
            return Err(CerseiError::Provider(message));
        }

        // An unterminated stream is not a clean turn (F-03c). `EndTurn` is only
        // legitimate when the provider actually ended the turn.
        let stop_reason = match &self.stop_reason {
            Some(sr) => sr.clone(),
            None if self.saw_terminal => StopReason::EndTurn,
            None => {
                return Err(CerseiError::Provider(format!(
                    "stream ended without a terminal event (no stop_reason, no message_stop); \
                     the response is incomplete ({} content block(s) accumulated)",
                    self.content_blocks.len()
                )))
            }
        };

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
            stop_reason,
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
