//! Plan mode tools: enter/exit read-only planning mode.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Global plan mode flag, shared across tool invocations.
static PLAN_MODE: AtomicBool = AtomicBool::new(false);

/// Check if plan mode is currently active.
pub fn is_plan_mode() -> bool {
    PLAN_MODE.load(Ordering::Relaxed)
}

/// Set plan mode state programmatically.
pub fn set_plan_mode(active: bool) {
    PLAN_MODE.store(active, Ordering::Relaxed);
}

pub struct EnterPlanModeTool;

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str { "EnterPlanMode" }
    fn description(&self) -> &str {
        "Enter plan mode: restricts to read-only tools for safe exploration and planning."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::None }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> ToolResult {
        PLAN_MODE.store(true, Ordering::Relaxed);
        ToolResult::success("Plan mode activated. Only read-only tools are available.")
    }
}

pub struct ExitPlanModeTool;

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str { "ExitPlanMode" }
    fn description(&self) -> &str {
        "Exit plan mode and return to full tool access."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::None }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> ToolResult {
        PLAN_MODE.store(false, Ordering::Relaxed);
        ToolResult::success("Plan mode deactivated. Full tool access restored.")
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
    async fn test_plan_mode_toggle() {
        set_plan_mode(false);
        assert!(!is_plan_mode());

        let enter = EnterPlanModeTool;
        enter.execute(serde_json::json!({}), &test_ctx()).await;
        assert!(is_plan_mode());

        let exit = ExitPlanModeTool;
        exit.execute(serde_json::json!({}), &test_ctx()).await;
        assert!(!is_plan_mode());
    }
}
