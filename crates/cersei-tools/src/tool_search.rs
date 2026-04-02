//! ToolSearch tool: search available tools by name or description.

use super::*;
use serde::Deserialize;

pub struct ToolSearchTool {
    tool_names: Vec<(String, String)>, // (name, description)
}

impl ToolSearchTool {
    pub fn new(tools: &[Box<dyn Tool>]) -> Self {
        Self {
            tool_names: tools
                .iter()
                .map(|t| (t.name().to_string(), t.description().to_string()))
                .collect(),
        }
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str { "ToolSearch" }
    fn description(&self) -> &str {
        "Search for available tools by keyword. Returns matching tool names and descriptions."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::None }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query (matches against tool names and descriptions)" }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input { query: String }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let query = input.query.to_lowercase();
        let matches: Vec<String> = self
            .tool_names
            .iter()
            .filter(|(name, desc)| {
                name.to_lowercase().contains(&query) || desc.to_lowercase().contains(&query)
            })
            .map(|(name, desc)| format!("- **{}**: {}", name, desc))
            .collect();

        if matches.is_empty() {
            ToolResult::success(format!("No tools found matching '{}'", input.query))
        } else {
            ToolResult::success(format!(
                "Found {} tool(s) matching '{}':\n{}",
                matches.len(),
                input.query,
                matches.join("\n")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_read::FileReadTool;
    use crate::file_write::FileWriteTool;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_search_file() {
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool),
            Box::new(FileWriteTool),
        ];
        let search = ToolSearchTool::new(&tools);
        let result = search.execute(serde_json::json!({"query": "file"}), &test_ctx()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("Read"));
        assert!(result.content.contains("Write"));
    }

    #[tokio::test]
    async fn test_search_no_match() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(FileReadTool)];
        let search = ToolSearchTool::new(&tools);
        let result = search.execute(serde_json::json!({"query": "xyz123"}), &test_ctx()).await;
        assert!(result.content.contains("No tools found"));
    }
}
