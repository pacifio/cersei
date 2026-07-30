//! Task system: create, track, update, and manage background tasks.
//!
//! Tasks represent long-running sub-agent work that runs asynchronously.
//! The coordinator can create tasks, check their status, and retrieve output.

use super::*;
use serde::{Deserialize, Serialize};

// ─── Task registry ───────────────────────────────────────────────────────────

static TASK_REGISTRY: once_cell::sync::Lazy<dashmap::DashMap<String, TaskEntry>> =
    once_cell::sync::Lazy::new(dashmap::DashMap::new);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEntry {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    pub output: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Stopped,
}

pub fn get_task(id: &str) -> Option<TaskEntry> {
    TASK_REGISTRY.get(id).map(|e| e.clone())
}

pub fn list_tasks() -> Vec<TaskEntry> {
    TASK_REGISTRY.iter().map(|e| e.value().clone()).collect()
}

pub fn clear_tasks() {
    TASK_REGISTRY.clear();
}

// ─── TaskCreate ──────────────────────────────────────────────────────────────

pub struct TaskCreateTool;

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "TaskCreate"
    }
    fn description(&self) -> &str {
        "Create a new task for tracking sub-agent work."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "What this task does" },
                "prompt": { "type": "string", "description": "The prompt for the sub-agent (optional)" }
            },
            "required": ["description"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Input {
            description: String,
            prompt: Option<String>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let task = TaskEntry {
            id: id.clone(),
            description: input.description.clone(),
            status: TaskStatus::Pending,
            output: None,
            created_at: now.clone(),
            updated_at: now,
            session_id: ctx.session_id.clone(),
        };
        TASK_REGISTRY.insert(id.clone(), task);
        ToolResult::success(format!("Task '{}' created: {}", id, input.description))
    }
}

// ─── TaskGet ─────────────────────────────────────────────────────────────────

pub struct TaskGetTool;

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> &str {
        "TaskGet"
    }
    fn description(&self) -> &str {
        "Get the status and output of a task by ID."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task ID" }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            id: String,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        match get_task(&input.id) {
            Some(task) => {
                let output = task.output.as_deref().unwrap_or("(no output yet)");
                ToolResult::success(format!(
                    "Task [{}] {:?}\n  {}\n  Output: {}",
                    task.id, task.status, task.description, output
                ))
            }
            None => ToolResult::error(format!("Task '{}' not found", input.id)),
        }
    }
}

// ─── TaskUpdate ──────────────────────────────────────────────────────────────

pub struct TaskUpdateTool;

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "TaskUpdate"
    }
    fn description(&self) -> &str {
        "Update a task's status and/or output."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task ID" },
                "status": { "type": "string", "enum": ["pending", "running", "completed", "failed", "stopped"] },
                "output": { "type": "string", "description": "Task output/result text" }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            id: String,
            status: Option<TaskStatus>,
            output: Option<String>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        match TASK_REGISTRY.get_mut(&input.id) {
            Some(mut entry) => {
                if let Some(status) = input.status {
                    entry.status = status;
                }
                if let Some(output) = input.output {
                    entry.output = Some(output);
                }
                entry.updated_at = chrono::Utc::now().to_rfc3339();
                ToolResult::success(format!("Task '{}' updated", input.id))
            }
            None => ToolResult::error(format!("Task '{}' not found", input.id)),
        }
    }
}

// ─── TaskList ────────────────────────────────────────────────────────────────

pub struct TaskListTool;

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "TaskList"
    }
    fn description(&self) -> &str {
        "List all tasks with their status."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}, "required": []})
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> ToolResult {
        let tasks = list_tasks();
        if tasks.is_empty() {
            return ToolResult::success("No tasks.");
        }
        let lines: Vec<String> = tasks
            .iter()
            .map(|t| {
                let status = format!("{:?}", t.status);
                format!("- [{}] {} — {}", t.id, status, t.description)
            })
            .collect();
        ToolResult::success(lines.join("\n"))
    }
}

// ─── TaskStop ────────────────────────────────────────────────────────────────

pub struct TaskStopTool;

#[async_trait]
impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "TaskStop"
    }
    fn description(&self) -> &str {
        "Stop/cancel a running task."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task ID to stop" }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            id: String,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        match TASK_REGISTRY.get_mut(&input.id) {
            Some(mut entry) => {
                entry.status = TaskStatus::Stopped;
                entry.updated_at = chrono::Utc::now().to_rfc3339();
                ToolResult::success(format!("Task '{}' stopped", input.id))
            }
            None => ToolResult::error(format!("Task '{}' not found", input.id)),
        }
    }
}

// ─── TaskOutput ──────────────────────────────────────────────────────────────

pub struct TaskOutputTool;

#[async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "TaskOutput"
    }
    fn description(&self) -> &str {
        "Get the full output of a completed task."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task ID" }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            id: String,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        match get_task(&input.id) {
            Some(task) => match &task.output {
                Some(output) => ToolResult::success(output.clone()),
                None => ToolResult::success("(no output yet)"),
            },
            None => ToolResult::error(format!("Task '{}' not found", input.id)),
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "task-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_task_full_lifecycle() {
        clear_tasks();
        let ctx = ToolContext {
            session_id: format!("task-lifecycle-{}", uuid::Uuid::new_v4()),
            ..test_ctx()
        };

        // Create
        let create = TaskCreateTool;
        let r = create
            .execute(serde_json::json!({"description": "Run tests"}), &ctx)
            .await;
        assert!(!r.is_error);
        // Extract ID from "Task 'XXXXXXXX' created: ..."
        let id = r.content.split('\'').nth(1).unwrap().to_string();

        // List
        let list = TaskListTool;
        let r = list.execute(serde_json::json!({}), &ctx).await;
        assert!(r.content.contains("Run tests"));

        // Update to running
        let update = TaskUpdateTool;
        update
            .execute(serde_json::json!({"id": &id, "status": "running"}), &ctx)
            .await;
        assert_eq!(get_task(&id).unwrap().status, TaskStatus::Running);

        // Update with output
        update
            .execute(
                serde_json::json!({
                    "id": &id,
                    "status": "completed",
                    "output": "All 42 tests passed"
                }),
                &ctx,
            )
            .await;
        let task = get_task(&id).unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(task.output.as_deref(), Some("All 42 tests passed"));

        // Get output
        let output = TaskOutputTool;
        let r = output.execute(serde_json::json!({"id": &id}), &ctx).await;
        assert!(r.content.contains("42 tests passed"));

        // Get status
        let get = TaskGetTool;
        let r = get.execute(serde_json::json!({"id": &id}), &ctx).await;
        assert!(r.content.contains("Completed"));
    }

    #[tokio::test]
    async fn test_task_stop() {
        let ctx = ToolContext {
            session_id: format!("stop-{}", uuid::Uuid::new_v4()),
            ..test_ctx()
        };

        let create = TaskCreateTool;
        let r = create
            .execute(serde_json::json!({"description": "Long task"}), &ctx)
            .await;
        let id = r.content.split('\'').nth(1).unwrap().to_string();

        let stop = TaskStopTool;
        stop.execute(serde_json::json!({"id": &id}), &ctx).await;
        assert_eq!(get_task(&id).unwrap().status, TaskStatus::Stopped);
    }
}
