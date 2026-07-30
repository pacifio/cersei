//! RemoteTrigger tool: fire cross-session events.

use super::*;
use serde::Deserialize;

/// Global event registry for cross-session triggers.
static TRIGGER_REGISTRY: once_cell::sync::Lazy<dashmap::DashMap<String, Vec<TriggerEvent>>> =
    once_cell::sync::Lazy::new(dashmap::DashMap::new);

#[derive(Debug, Clone, serde::Serialize)]
pub struct TriggerEvent {
    pub source_session: String,
    pub target_session: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub timestamp: String,
}

/// Drain pending trigger events for a session.
pub fn drain_triggers(session_id: &str) -> Vec<TriggerEvent> {
    TRIGGER_REGISTRY
        .remove(session_id)
        .map(|(_, v)| v)
        .unwrap_or_default()
}

pub struct RemoteTriggerTool;

#[async_trait]
impl Tool for RemoteTriggerTool {
    fn name(&self) -> &str {
        "RemoteTrigger"
    }
    fn description(&self) -> &str {
        "Send an event to another session or agent."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target_session": { "type": "string", "description": "Target session ID" },
                "event_type": { "type": "string", "description": "Event type identifier" },
                "payload": { "description": "Event payload (any JSON)" }
            },
            "required": ["target_session", "event_type"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            target_session: String,
            event_type: String,
            payload: Option<Value>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let event = TriggerEvent {
            source_session: ctx.session_id.clone(),
            target_session: input.target_session.clone(),
            event_type: input.event_type.clone(),
            payload: input.payload.unwrap_or(Value::Null),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        TRIGGER_REGISTRY
            .entry(input.target_session.clone())
            .or_default()
            .push(event);

        ToolResult::success(format!(
            "Trigger '{}' sent to session '{}'",
            input.event_type, input.target_session
        ))
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
            session_id: "sender".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_trigger_send_receive() {
        let tool = RemoteTriggerTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "target_session": "receiver",
                    "event_type": "tests_complete",
                    "payload": {"passed": 42}
                }),
                &test_ctx(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("sent"));

        let events = drain_triggers("receiver");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "tests_complete");
        assert_eq!(events[0].source_session, "sender");
    }
}
