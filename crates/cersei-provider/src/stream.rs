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

// These tests bind the shared accumulator itself, not any one provider's SSE
// reader. `sse_pathologies` covers the openai.rs copy of the F-03 logic, but
// Anthropic and Gemini stream through this struct, and the mutation audit
// (TOOL-CALLING-RELIABILITY.md §10.3) showed both F-03 halves here could be
// reverted with the whole workspace suite green.
#[cfg(test)]
mod tests {
    use super::*;

    fn tool_use_events(json_deltas: &[&str]) -> Vec<StreamEvent> {
        let mut events = vec![
            StreamEvent::MessageStart {
                id: "msg_1".into(),
                model: "test-model".into(),
            },
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: "tool_use".into(),
                id: Some("tu_1".into()),
                name: Some("Read".into()),
            },
        ];
        for d in json_deltas {
            events.push(StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: (*d).into(),
            });
        }
        events.push(StreamEvent::ContentBlockStop { index: 0 });
        events
    }

    fn accumulate(events: Vec<StreamEvent>) -> StreamAccumulator {
        let mut acc = StreamAccumulator::new();
        for e in events {
            acc.process_event(e);
        }
        acc
    }

    fn sole_tool_use_input(response: &crate::CompletionResponse) -> serde_json::Value {
        match &response.message.content {
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .find_map(|b| match b {
                    ContentBlock::ToolUse { input, .. } => Some(input.clone()),
                    _ => None,
                })
                .expect("response must contain a tool_use block"),
            other => panic!("expected block content, got {other:?}"),
        }
    }

    // ── F-03b: a mid-stream provider error must fail the turn ──

    #[test]
    fn stream_error_fails_the_turn_even_with_a_terminal_event() {
        let mut events = tool_use_events(&[r#"{"file_path":"/a.rs"}"#]);
        events.push(StreamEvent::Error {
            message: "provider exploded mid-stream".into(),
        });
        // A terminal event after the error must not launder it into success.
        events.push(StreamEvent::MessageStop);

        let err = accumulate(events)
            .into_response()
            .expect_err("an errored stream must not become a clean response");
        assert!(
            err.to_string().contains("provider exploded mid-stream"),
            "the provider's message is the only diagnostic: {err}"
        );
    }

    #[test]
    fn first_stream_error_wins_over_cascade_noise() {
        let mut events = tool_use_events(&[]);
        events.push(StreamEvent::Error {
            message: "root cause".into(),
        });
        events.push(StreamEvent::Error {
            message: "cascade noise".into(),
        });
        events.push(StreamEvent::MessageStop);

        let err = accumulate(events).into_response().expect_err("must fail");
        assert!(
            err.to_string().contains("root cause"),
            "the first error is the diagnostic one, got: {err}"
        );
    }

    // ── F-03c: a stream that just stops is not a clean EndTurn ──

    #[test]
    fn terminal_less_stream_is_an_error_not_end_turn() {
        // Identical to a healthy tool-call stream except no MessageStop and no
        // stop_reason ever arrive: the connection was cut mid-flight.
        let events = tool_use_events(&[r#"{"file_path":"/a.rs"}"#]);

        let err = accumulate(events)
            .into_response()
            .expect_err("an unterminated stream must not report a clean turn");
        assert!(
            err.to_string().contains("terminal"),
            "the error must say the stream never terminated: {err}"
        );
    }

    #[test]
    fn message_stop_alone_is_a_clean_end_turn() {
        // Control: the same stream with a proper terminal event succeeds.
        let mut events = tool_use_events(&[r#"{"file_path":"/a.rs"}"#]);
        events.push(StreamEvent::MessageStop);

        let response = accumulate(events)
            .into_response()
            .expect("a properly terminated stream is a valid response");
        assert_eq!(response.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn stop_reason_via_message_delta_counts_as_terminal() {
        // Providers may send stop_reason in a MessageDelta and never a
        // MessageStop; that is still a provider-ended turn.
        let mut events = tool_use_events(&[r#"{"file_path":"/a.rs"}"#]);
        events.push(StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });

        let response = accumulate(events)
            .into_response()
            .expect("stop_reason is a terminal signal");
        assert_eq!(response.stop_reason, StopReason::ToolUse);
    }

    // ── F-05a: a no-argument call is `{}`, never `null` ──

    #[test]
    fn tool_use_with_no_argument_deltas_yields_empty_object() {
        let mut events = tool_use_events(&[]);
        events.push(StreamEvent::MessageStop);

        let response = accumulate(events).into_response().expect("valid stream");
        assert_eq!(
            sole_tool_use_input(&response),
            serde_json::json!({}),
            "zero input_json_delta events is a no-argument call, not null"
        );
    }

    #[test]
    fn literal_null_arguments_yield_empty_object() {
        let mut events = tool_use_events(&["null"]);
        events.push(StreamEvent::MessageStop);

        let response = accumulate(events).into_response().expect("valid stream");
        assert_eq!(
            sole_tool_use_input(&response),
            serde_json::json!({}),
            "a literal `null` payload is a no-argument call, not a type error"
        );
    }

    #[test]
    fn whitespace_only_arguments_yield_empty_object() {
        let mut events = tool_use_events(&["  \n"]);
        events.push(StreamEvent::MessageStop);

        let response = accumulate(events).into_response().expect("valid stream");
        assert_eq!(sole_tool_use_input(&response), serde_json::json!({}));
    }

    // ── F-05b at this seam: malformed JSON keeps the raw text and the error ──

    #[test]
    fn malformed_arguments_preserve_raw_text_and_parse_error() {
        let raw = r#"{'file_path': '/a.rs'}"#; // single quotes: invalid JSON
        let mut events = tool_use_events(&[raw]);
        events.push(StreamEvent::MessageStop);

        let response = accumulate(events).into_response().expect("valid stream");
        let input = sole_tool_use_input(&response);
        assert_eq!(
            input["__raw"].as_str(),
            Some(raw),
            "the model's exact bytes must survive so dispatch can echo them"
        );
        assert!(
            input["__parse_error"].as_str().is_some_and(|e| !e.is_empty()),
            "the parse error must survive alongside the raw text: {input}"
        );
    }
}
