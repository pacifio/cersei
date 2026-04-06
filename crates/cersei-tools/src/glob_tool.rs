//! Glob tool: find files by pattern.

use super::*;
use crate::tool_primitives::search as psearch;
use serde::Deserialize;

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "Glob" }
    fn description(&self) -> &str { "Find files matching a glob pattern." }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::ReadOnly }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.rs)" },
                "path": { "type": "string", "description": "Directory to search in" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            pattern: String,
            path: Option<String>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let base_dir = input
            .path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.working_dir.clone());

        match psearch::glob(&input.pattern, &base_dir).await {
            Ok(mut paths) => {
                paths.sort();
                if paths.is_empty() {
                    ToolResult::success("No files matched the pattern.")
                } else {
                    let output: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                    ToolResult::success(output.join("\n"))
                }
            }
            Err(e) => ToolResult::error(format!("Glob failed: {}", e)),
        }
    }
}
