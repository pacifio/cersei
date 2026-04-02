//! File edit tool: performs exact string replacements.

use super::*;
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
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
        };

        if !content.contains(&input.old_string) {
            return ToolResult::error(format!(
                "old_string not found in {}",
                input.file_path
            ));
        }

        let new_content = if input.replace_all {
            content.replace(&input.old_string, &input.new_string)
        } else {
            let count = content.matches(&input.old_string).count();
            if count > 1 {
                return ToolResult::error(format!(
                    "old_string is not unique ({} occurrences). Use replace_all or provide more context.",
                    count
                ));
            }
            content.replacen(&input.old_string, &input.new_string, 1)
        };

        match tokio::fs::write(path, &new_content).await {
            Ok(()) => ToolResult::success(format!(
                "The file {} has been updated successfully.",
                input.file_path
            )),
            Err(e) => ToolResult::error(format!("Failed to write file: {}", e)),
        }
    }
}
