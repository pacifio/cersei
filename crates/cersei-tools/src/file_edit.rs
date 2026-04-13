//! File edit tool: performs exact string replacements.

use super::*;
use crate::tool_primitives::fs as pfs;
use serde::Deserialize;

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "Edit" }
    fn description(&self) -> &str { "Perform exact string replacements in files." }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::Write }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file" },
                "old_string": { "type": "string", "description": "The text to replace" },
                "new_string": { "type": "string", "description": "The replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences", "default": false }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            file_path: String,
            old_string: String,
            new_string: String,
            #[serde(default)]
            replace_all: bool,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let path = std::path::Path::new(&input.file_path);

        // Capture content before edit for diff
        let before_content = tokio::fs::read_to_string(path).await.unwrap_or_default();

        match pfs::edit_file(path, &input.old_string, &input.new_string, input.replace_all).await {
            Ok(result) => {
                // Generate a compact inline diff
                let after_content = tokio::fs::read_to_string(path).await.unwrap_or_default();
                let diff = crate::tool_primitives::diff::unified_diff(
                    &before_content, &after_content, 2,
                );

                // Include diff in result (truncated for large changes)
                let diff_preview = if diff.lines().count() > 30 {
                    let truncated: String = diff.lines().take(25).collect::<Vec<_>>().join("\n");
                    format!("{}\n... ({} more lines)", truncated, diff.lines().count() - 25)
                } else {
                    diff
                };

                ToolResult::success(format!(
                    "The file {} has been updated. {} replacement(s) made.\n{}",
                    input.file_path, result.replacements_made, diff_preview
                ))
            }
            Err(pfs::EditError::NotFound) => ToolResult::error(format!(
                "old_string not found in {}", input.file_path
            )),
            Err(pfs::EditError::AmbiguousMatch { count }) => ToolResult::error(format!(
                "old_string is not unique ({} occurrences). Use replace_all or provide more context.",
                count
            )),
            Err(pfs::EditError::Io(e)) => ToolResult::error(format!(
                "Failed to edit file: {}", e
            )),
        }
    }
}
