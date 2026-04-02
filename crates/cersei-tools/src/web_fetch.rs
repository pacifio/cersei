//! WebFetch tool: fetch and parse web page content.

use super::*;
use serde::Deserialize;

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "WebFetch" }
    fn description(&self) -> &str {
        "Fetch a URL and return its content as readable text. HTML is converted to markdown."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::ReadOnly }
    fn category(&self) -> ToolCategory { ToolCategory::Web }

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

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Cersei-Agent/0.1")
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to create HTTP client: {}", e)),
        };

        let response = match client.get(&input.url).send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Fetch failed: {}", e)),
        };

        let status = response.status();
        if !status.is_success() {
            return ToolResult::error(format!("HTTP {}: {}", status.as_u16(), status.as_str()));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = match response.text().await {
            Ok(t) => t,
            Err(e) => return ToolResult::error(format!("Failed to read body: {}", e)),
        };

        // Convert HTML to readable text
        let text = if content_type.contains("html") {
            html2text::from_read(body.as_bytes(), 80)
        } else {
            body
        };

        // Truncate if needed
        let text = if text.len() > max_chars {
            format!(
                "{}\n\n[Truncated: {} chars total, showing first {}]",
                &text[..max_chars],
                text.len(),
                max_chars
            )
        } else {
            text
        };

        ToolResult::success(text)
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
