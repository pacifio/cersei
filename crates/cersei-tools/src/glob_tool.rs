//! Glob tool: find files by pattern.

use super::*;
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

        let full_pattern = base_dir.join(&input.pattern).display().to_string();

        match glob::glob(&full_pattern) {
            Ok(entries) => {
                let mut matches: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|p| p.display().to_string())
                    .collect();
                matches.sort();

                if matches.is_empty() {
                    ToolResult::success("No files matched the pattern.")
                } else {
                    ToolResult::success(matches.join("\n"))
                }
            }
            Err(e) => ToolResult::error(format!("Invalid glob pattern: {}", e)),
        }
    }
}
