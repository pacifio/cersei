//! WebFetch tool: fetch and parse web page content.

use super::*;
use crate::tool_primitives::http as phttp;
use serde::Deserialize;

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "WebFetch"
    }
    fn description(&self) -> &str {
        "Fetch a URL and return its content as readable text. HTML is converted to markdown."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Web
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The URL to fetch" },
                "max_chars": { "type": "integer", "description": "Max characters to return (default 50000)" }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            url: String,
            max_chars: Option<usize>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let max_chars = input.max_chars.unwrap_or(50_000);

        match phttp::fetch_html(&input.url, max_chars, phttp::HttpOptions::default()).await {
            Ok(text) => {
                if text.len() >= max_chars {
                    ToolResult::success(format!(
                        "{}\n\n[Truncated: showing first {} chars]",
                        text, max_chars
                    ))
                } else {
                    ToolResult::success(text)
                }
            }
            Err(e) => ToolResult::error(format!("Fetch failed: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = WebFetchTool;
        let schema = tool.input_schema();
        assert!(schema["properties"]["url"].is_object());
        assert_eq!(tool.permission_level(), PermissionLevel::ReadOnly);
        assert_eq!(tool.category(), ToolCategory::Web);
    }
}
