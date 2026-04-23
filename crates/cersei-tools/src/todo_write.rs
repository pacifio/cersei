//! TodoWrite tool: structured task list management.

use super::*;
use serde::{Deserialize, Serialize};

/// Global todo storage keyed by session_id.
static TODO_REGISTRY: once_cell::sync::Lazy<dashmap::DashMap<String, Vec<TodoItem>>> =
    once_cell::sync::Lazy::new(dashmap::DashMap::new);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    #[serde(rename = "activeForm")]
    pub active_form: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// Get the current todo list for a session.
pub fn get_todos(session_id: &str) -> Vec<TodoItem> {
    TODO_REGISTRY
        .get(session_id)
        .map(|v| v.clone())
        .unwrap_or_default()
}

/// Clear todos for a session.
pub fn clear_todos(session_id: &str) {
    TODO_REGISTRY.remove(session_id);
}

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }
    fn description(&self) -> &str {
        "Create and manage a structured task list. Tracks progress with pending/in_progress/completed states."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete updated todo list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Task description (imperative form)" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "activeForm": { "type": "string", "description": "Present continuous form (e.g., 'Running tests')" }
                        },
                        "required": ["content", "status", "activeForm"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            todos: Vec<TodoItem>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        TODO_REGISTRY.insert(ctx.session_id.clone(), input.todos.clone());

        let summary: Vec<String> = input
            .todos
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let icon = match t.status {
                    TodoStatus::Completed => "[x]",
                    TodoStatus::InProgress => "[>]",
                    TodoStatus::Pending => "[ ]",
                };
                format!("{}. {} {}", i + 1, icon, t.content)
            })
            .collect();

        ToolResult::success(format!(
            "Todos updated ({} items):\n{}",
            input.todos.len(),
            summary.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "todo-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_todo_create_and_read() {
        clear_todos("todo-test");
        let tool = TodoWriteTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        {"content": "Build feature", "status": "in_progress", "activeForm": "Building feature"},
                        {"content": "Write tests", "status": "pending", "activeForm": "Writing tests"}
                    ]
                }),
                &test_ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("2 items"));

        let todos = get_todos("todo-test");
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].status, TodoStatus::InProgress);
        assert_eq!(todos[1].status, TodoStatus::Pending);
    }

    #[tokio::test]
    async fn test_todo_update() {
        clear_todos("todo-test2");
        let tool = TodoWriteTool;
        let ctx = ToolContext {
            session_id: "todo-test2".into(),
            ..test_ctx()
        };

        // Create
        tool.execute(
            serde_json::json!({
                "todos": [{"content": "Task A", "status": "pending", "activeForm": "Doing A"}]
            }),
            &ctx,
        )
        .await;

        // Update to completed
        tool.execute(
            serde_json::json!({
                "todos": [{"content": "Task A", "status": "completed", "activeForm": "Doing A"}]
            }),
            &ctx,
        )
        .await;

        let todos = get_todos("todo-test2");
        assert_eq!(todos[0].status, TodoStatus::Completed);
    }
}
