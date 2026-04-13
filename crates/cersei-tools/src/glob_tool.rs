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
                "path": { "type": "string", "description": "Directory to search in" },
                "limit": { "type": "integer", "description": "Max results to return (default 200)" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            pattern: String,
            path: Option<String>,
            limit: Option<usize>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let max_results = input.limit.unwrap_or(200);

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
                    let total = paths.len();
                    let truncated = total > max_results;
                    let output: Vec<String> = paths.iter().take(max_results).map(|p| p.display().to_string()).collect();
                    let mut result = output.join("\n");
                    if truncated {
                        result.push_str(&format!(
                            "\n\n[Showing {max_results} of {total} matches. Use a more specific pattern to narrow results.]"
                        ));
                    }
                    ToolResult::success(result)
                }
            }
            Err(e) => ToolResult::error(format!("Glob failed: {}", e)),
        }
    }
}
