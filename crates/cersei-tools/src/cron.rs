//! Cron tools: schedule, list, and delete recurring/one-shot tasks.

use super::*;
use serde::{Deserialize, Serialize};

static CRON_REGISTRY: once_cell::sync::Lazy<dashmap::DashMap<String, CronEntry>> =
    once_cell::sync::Lazy::new(dashmap::DashMap::new);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronEntry {
    pub id: String,
    pub schedule: String,
    pub prompt: String,
    pub created_at: String,
    pub last_run: Option<String>,
    pub run_count: u32,
}

pub fn list_crons() -> Vec<CronEntry> {
    CRON_REGISTRY.iter().map(|e| e.value().clone()).collect()
}

/// Ids of every scheduled job, for "not found" feedback (F-A15).
fn cron_ids() -> Vec<String> {
    CRON_REGISTRY.iter().map(|e| e.key().clone()).collect()
}

pub fn clear_crons() {
    CRON_REGISTRY.clear();
}

// ─── CronCreate ──────────────────────────────────────────────────────────────

pub struct CronCreateTool;

#[async_trait]
impl Tool for CronCreateTool {
    fn name(&self) -> &str {
        "CronCreate"
    }
    fn description(&self) -> &str {
        "Schedule a recurring or one-shot prompt to run on a cron schedule."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "schedule": { "type": "string", "description": "Cron expression (e.g. '*/5 * * * *' or 'once:30s')" },
                "prompt": { "type": "string", "description": "The prompt to execute on schedule" }
            },
            "required": ["schedule", "prompt"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            schedule: String,
            prompt: String,
        }

        let input: Input = match crate::tool_feedback::parse_input(self, &input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let entry = CronEntry {
            id: id.clone(),
            schedule: input.schedule.clone(),
            prompt: input.prompt.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_run: None,
            run_count: 0,
        };
        CRON_REGISTRY.insert(id.clone(), entry);

        ToolResult::success(format!(
            "Cron job '{}' created: {} → {}",
            id, input.schedule, input.prompt
        ))
    }
}

// ─── CronList ────────────────────────────────────────────────────────────────

pub struct CronListTool;

#[async_trait]
impl Tool for CronListTool {
    fn name(&self) -> &str {
        "CronList"
    }
    fn description(&self) -> &str {
        "List all scheduled cron jobs with their IDs, schedules, and run counts."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}, "required": []})
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> ToolResult {
        let entries = list_crons();
        if entries.is_empty() {
            return ToolResult::success("No cron jobs scheduled.");
        }
        let lines: Vec<String> = entries
            .iter()
            .map(|e| {
                format!(
                    "- [{}] {} → {} (runs: {})",
                    e.id, e.schedule, e.prompt, e.run_count
                )
            })
            .collect();
        ToolResult::success(lines.join("\n"))
    }
}

// ─── CronDelete ──────────────────────────────────────────────────────────────

pub struct CronDeleteTool;

#[async_trait]
impl Tool for CronDeleteTool {
    fn name(&self) -> &str {
        "CronDelete"
    }
    fn description(&self) -> &str {
        "Delete a scheduled cron job by ID (CronList shows the IDs)."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Cron job ID to delete" }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            id: String,
        }

        let input: Input = match crate::tool_feedback::parse_input(self, &input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        if CRON_REGISTRY.remove(&input.id).is_some() {
            ToolResult::success(format!("Cron job '{}' deleted.", input.id))
        } else {
            // F-A15: list the job ids that do exist.
            crate::tool_feedback::not_found(
                "cron job",
                &input.id,
                &cron_ids(),
                "Run CronList to see every scheduled job.",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "cron-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_cron_lifecycle() {
        clear_crons();

        let create = CronCreateTool;
        let result = create
            .execute(
                serde_json::json!({
                    "schedule": "*/5 * * * *",
                    "prompt": "Run tests"
                }),
                &test_ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("created"));

        let list = CronListTool;
        let result = list.execute(serde_json::json!({}), &test_ctx()).await;
        assert!(result.content.contains("Run tests"));

        let entries = list_crons();
        assert_eq!(entries.len(), 1);
        let id = entries[0].id.clone();

        let delete = CronDeleteTool;
        let result = delete
            .execute(serde_json::json!({"id": id}), &test_ctx())
            .await;
        assert!(!result.is_error);

        assert!(list_crons().is_empty());
    }
}

#[cfg(test)]
mod not_found_tests {
    use super::*;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "cron-nf".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    /// F-A15. Deliberately does not create or clear cron jobs: CRON_REGISTRY
    /// is a process-wide static and `test_cron_lifecycle` asserts on its exact
    /// length, so any mutation here would race it.
    #[tokio::test]
    async fn cron_delete_not_found_points_at_the_real_ids() {
        let r = CronDeleteTool
            .execute(serde_json::json!({ "id": "deadbeef" }), &ctx())
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("not found"), "{}", r.content);
        assert!(
            r.content.contains("Available cron jobs (")
                || r.content.contains("no cron jobs available"),
            "must enumerate or state emptiness: {}",
            r.content
        );
        assert!(r.content.contains("CronList"), "{}", r.content);
    }
}
