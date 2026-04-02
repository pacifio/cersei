//! File read tool.

use super::*;
use serde::Deserialize;

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "Read" }
    fn description(&self) -> &str { "Read a file from the filesystem." }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::ReadOnly }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file" },
                "offset": { "type": "integer", "description": "Line number to start reading from" },
                "limit": { "type": "integer", "description": "Number of lines to read" }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            file_path: String,
            offset: Option<usize>,
            limit: Option<usize>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let path = std::path::Path::new(&input.file_path);
        if !path.exists() {
            return ToolResult::error(format!("File not found: {}", input.file_path));
        }

        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let offset = input.offset.unwrap_or(0);
                let limit = input.limit.unwrap_or(2000);

                let selected: Vec<String> = lines
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .enumerate()
                    .map(|(i, line)| format!("{:>6}\t{}", offset + i + 1, line))
                    .collect();

                ToolResult::success(selected.join("\n"))
            }
            Err(e) => ToolResult::error(format!("Failed to read file: {}", e)),
        }
    }
}
