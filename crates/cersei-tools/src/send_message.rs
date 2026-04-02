//! SendMessage tool: inter-agent message passing.

use super::*;
use serde::Deserialize;

/// Global inbox registry keyed by session_id.
static INBOX_REGISTRY: once_cell::sync::Lazy<
    dashmap::DashMap<String, Vec<InboxMessage>>,
> = once_cell::sync::Lazy::new(dashmap::DashMap::new);

#[derive(Debug, Clone, serde::Serialize)]
pub struct InboxMessage {
    pub from: String,
    pub content: String,
    pub timestamp: String,
}

/// Drain all pending messages for a session.
pub fn drain_inbox(session_id: &str) -> Vec<InboxMessage> {
    INBOX_REGISTRY
        .remove(session_id)
        .map(|(_, v)| v)
        .unwrap_or_default()
}

/// Peek at pending messages without consuming them.
pub fn peek_inbox(session_id: &str) -> Vec<InboxMessage> {
    INBOX_REGISTRY
        .get(session_id)
        .map(|v| v.clone())
        .unwrap_or_default()
}

pub struct SendMessageTool;

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str { "SendMessage" }
    fn description(&self) -> &str { "Send a message to another agent or session by ID." }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::None }
    fn category(&self) -> ToolCategory { ToolCategory::Orchestration }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "Target session/agent ID" },
                "content": { "type": "string", "description": "Message content" }
            },
            "required": ["to", "content"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input { to: String, content: String }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let msg = InboxMessage {
            from: ctx.session_id.clone(),
            content: input.content.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        INBOX_REGISTRY
            .entry(input.to.clone())
            .or_default()
            .push(msg);

        ToolResult::success(format!("Message sent to '{}'", input.to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "agent-a".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_send_and_receive() {
        let tool = SendMessageTool;
        tool.execute(serde_json::json!({"to": "agent-b", "content": "Hello B!"}), &test_ctx()).await;

        let msgs = peek_inbox("agent-b");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].from, "agent-a");
        assert_eq!(msgs[0].content, "Hello B!");

        let drained = drain_inbox("agent-b");
        assert_eq!(drained.len(), 1);
        assert!(peek_inbox("agent-b").is_empty());
    }
}
