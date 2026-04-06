//! Grep tool: search file contents with regex.

use super::*;
use crate::tool_primitives::search as psearch;
use serde::Deserialize;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "Grep" }
    fn description(&self) -> &str { "Search file contents using regex patterns." }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::ReadOnly }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "File or directory to search in" },
                "glob": { "type": "string", "description": "Glob pattern to filter files" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            pattern: String,
            path: Option<String>,
            glob: Option<String>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let search_path = input
            .path
            .unwrap_or_else(|| ctx.working_dir.display().to_string());

        let opts = psearch::GrepOptions {
            glob_filter: input.glob,
            max_results: Some(250),
            case_insensitive: false,
        };

        match psearch::grep(&input.pattern, std::path::Path::new(&search_path), opts).await {
            Ok(matches) => {
                if matches.is_empty() {
                    ToolResult::success("No matches found.")
                } else {
                    let output: Vec<String> = matches
                        .iter()
                        .map(|m| format!("{}:{}:{}", m.file.display(), m.line_number, m.line_content))
                        .collect();
                    ToolResult::success(output.join("\n"))
                }
            }
            Err(e) => ToolResult::error(format!("Search failed: {}", e)),
        }
    }
}
