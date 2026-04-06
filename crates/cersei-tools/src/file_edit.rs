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
        match pfs::edit_file(path, &input.old_string, &input.new_string, input.replace_all).await {
            Ok(result) => ToolResult::success(format!(
                "The file {} has been updated successfully. {} replacement(s) made.",
                input.file_path, result.replacements_made
            )),
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
