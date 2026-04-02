//! Worktree tools: create/exit isolated git worktrees for parallel work.

use super::*;
use serde::Deserialize;
use std::process::Stdio;

pub struct EnterWorktreeTool;

#[async_trait]
impl Tool for EnterWorktreeTool {
    fn name(&self) -> &str { "EnterWorktree" }
    fn description(&self) -> &str {
        "Create an isolated git worktree for the agent to work in without affecting the main branch."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::Write }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "branch": { "type": "string", "description": "Branch name for the worktree" },
                "path": { "type": "string", "description": "Optional path for the worktree (default: auto-generated)" }
            },
            "required": ["branch"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            branch: String,
            path: Option<String>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let worktree_path = input.path.unwrap_or_else(|| {
            let tmp = std::env::temp_dir().join(format!("cersei-wt-{}", &input.branch));
            tmp.display().to_string()
        });

        let output = tokio::process::Command::new("git")
            .args(["worktree", "add", "-b", &input.branch, &worktree_path])
            .current_dir(&ctx.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(o) if o.status.success() => {
                ToolResult::success(format!(
                    "Worktree created at: {}\nBranch: {}",
                    worktree_path, input.branch
                ))
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                ToolResult::error(format!("git worktree failed: {}", stderr))
            }
            Err(e) => ToolResult::error(format!("Failed to run git: {}", e)),
        }
    }
}

pub struct ExitWorktreeTool;

#[async_trait]
impl Tool for ExitWorktreeTool {
    fn name(&self) -> &str { "ExitWorktree" }
    fn description(&self) -> &str { "Remove a git worktree." }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::Write }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path of the worktree to remove" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input { path: String }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let output = tokio::process::Command::new("git")
            .args(["worktree", "remove", "--force", &input.path])
            .current_dir(&ctx.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(o) if o.status.success() => {
                ToolResult::success(format!("Worktree removed: {}", input.path))
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                ToolResult::error(format!("git worktree remove failed: {}", stderr))
            }
            Err(e) => ToolResult::error(format!("Failed to run git: {}", e)),
        }
    }
}
