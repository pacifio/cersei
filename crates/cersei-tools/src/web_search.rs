//! WebSearch tool: search the web via a configurable search API.

use super::*;
use serde::Deserialize;

/// Environment variable for search API key.
const SEARCH_API_KEY_ENV: &str = "CERSEI_SEARCH_API_KEY";
/// Environment variable for search API endpoint.
const SEARCH_API_URL_ENV: &str = "CERSEI_SEARCH_API_URL";
/// Default search endpoint (Brave Search API).
const DEFAULT_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "WebSearch"
    }
    fn description(&self) -> &str {
        "Search the web and return relevant results. Requires CERSEI_SEARCH_API_KEY environment variable."
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
                "query": { "type": "string", "description": "Search query" },
                "num_results": { "type": "integer", "description": "Number of results (default 8, max 20)" }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            query: String,
            num_results: Option<usize>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let api_key = match std::env::var(SEARCH_API_KEY_ENV) {
            Ok(k) if !k.is_empty() => k,
            _ => {
                return ToolResult::error(format!(
                    "Web search requires {}. Set it to your Brave Search API key.",
                    SEARCH_API_KEY_ENV
                ))
            }
        };

        let search_url =
            std::env::var(SEARCH_API_URL_ENV).unwrap_or_else(|_| DEFAULT_SEARCH_URL.to_string());
        let num_results = input.num_results.unwrap_or(8).min(20);

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("HTTP client error: {}", e)),
        };

        let response = match client
            .get(&search_url)
            .header("X-Subscription-Token", &api_key)
            .header("Accept", "application/json")
            .query(&[
                ("q", input.query.as_str()),
                ("count", &num_results.to_string()),
            ])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Search request failed: {}", e)),
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return ToolResult::error(format!("Search API error ({}): {}", status, body));
        }

        let json: Value = match response.json().await {
            Ok(j) => j,
            Err(e) => return ToolResult::error(format!("Failed to parse response: {}", e)),
        };

        // Format results
        let mut output = String::new();
        if let Some(results) = json["web"]["results"].as_array() {
            for (i, result) in results.iter().enumerate().take(num_results) {
                let title = result["title"].as_str().unwrap_or("(no title)");
                let url = result["url"].as_str().unwrap_or("");
                let desc = result["description"].as_str().unwrap_or("");
                output.push_str(&format!(
                    "{}. **{}**\n   {}\n   {}\n\n",
                    i + 1,
                    title,
                    url,
                    desc
                ));
            }
        }

        if output.is_empty() {
            ToolResult::success(format!("No results found for: {}", input.query))
        } else {
            ToolResult::success(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = WebSearchTool;
        assert!(tool.input_schema()["properties"]["query"].is_object());
        assert_eq!(tool.category(), ToolCategory::Web);
    }
}
