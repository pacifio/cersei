//! AskUserQuestion tool: prompt the user for input during agent execution.

use super::*;
use serde::Deserialize;

pub struct AskUserQuestionTool;

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> &str { "AskUserQuestion" }
    fn description(&self) -> &str {
        "Ask the user a question and wait for their response. Use when you need clarification or input."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::None }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "The question to ask the user" }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input { question: String }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        // In headless/SDK mode, return the question as-is for the caller to handle
        // via the event system (AgentEvent::PermissionRequired or similar).
        // In interactive mode, a TUI would intercept this and prompt the user.
        ToolResult::success(format!(
            "[Question for user]: {}\n\n(Waiting for user response via event handler)",
            input.question
        ))
    }
}
