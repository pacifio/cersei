//! SyntheticOutput tool: return structured JSON data for SDK/coordinator sessions.

use super::*;

pub struct SyntheticOutputTool;

#[async_trait]
impl Tool for SyntheticOutputTool {
    fn name(&self) -> &str {
        "SyntheticOutput"
    }
    fn description(&self) -> &str {
        "Return structured JSON output for programmatic consumption. Used by coordinator mode and SDK integrations."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "data": {
                    "description": "Structured data to return (any valid JSON)"
                }
            },
            "required": ["data"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let data = input.get("data").cloned().unwrap_or(Value::Null);
        ToolResult::success(serde_json::to_string_pretty(&data).unwrap_or_default())
            .with_metadata(serde_json::json!({"type": "synthetic_output", "data": data}))
    }
}
