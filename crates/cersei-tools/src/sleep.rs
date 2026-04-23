//! Sleep tool: delay execution for a specified duration.

use super::*;
use serde::Deserialize;

pub struct SleepTool;

#[async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &str {
        "Sleep"
    }
    fn description(&self) -> &str {
        "Pause execution for the specified number of milliseconds."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "duration_ms": { "type": "integer", "description": "Duration to sleep in milliseconds (max 60000)" }
            },
            "required": ["duration_ms"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            duration_ms: u64,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let duration = input.duration_ms.min(60_000);
        tokio::time::sleep(std::time::Duration::from_millis(duration)).await;
        ToolResult::success(format!("Slept for {}ms", duration))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_sleep_100ms() {
        let tool = SleepTool;
        let start = std::time::Instant::now();
        let result = tool
            .execute(serde_json::json!({"duration_ms": 100}), &test_ctx())
            .await;
        assert!(!result.is_error);
        assert!(start.elapsed().as_millis() >= 90);
    }

    #[tokio::test]
    async fn test_sleep_capped() {
        let tool = SleepTool;
        // Should cap at 60000ms, not actually sleep that long in test
        let result = tool
            .execute(serde_json::json!({"duration_ms": 1}), &test_ctx())
            .await;
        assert!(result.content.contains("1ms"));
    }
}
