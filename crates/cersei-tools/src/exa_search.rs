//! ExaSearch tool: AI-powered web search via the Exa API (https://exa.ai).

use super::*;
use serde::{Deserialize, Serialize};

/// Environment variable for the Exa API key.
const EXA_API_KEY_ENV: &str = "EXA_API_KEY";
/// Exa search endpoint.
const EXA_SEARCH_URL: &str = "https://api.exa.ai/search";

pub struct ExaSearchTool;

// ─── Request types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExaSearchRequest {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_results: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contents: Option<ExaContents>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_published_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_published_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_location: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExaContents {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<ExaTextOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    highlights: Option<ExaHighlightsOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<ExaSummaryOptions>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExaTextOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_characters: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExaHighlightsOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_characters: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExaSummaryOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
}

// ─── Response types ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExaSearchResponse {
    results: Vec<ExaResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExaResult {
    title: Option<String>,
    url: String,
    published_date: Option<String>,
    author: Option<String>,
    text: Option<String>,
    highlights: Option<Vec<String>>,
    summary: Option<String>,
}

// ─── Tool input ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Input {
    query: String,
    search_type: Option<String>,
    num_results: Option<usize>,
    category: Option<String>,
    content_mode: Option<String>,
    max_characters: Option<usize>,
    include_domains: Option<Vec<String>>,
    exclude_domains: Option<Vec<String>>,
    start_published_date: Option<String>,
    end_published_date: Option<String>,
    user_location: Option<String>,
}

// ─── Tool implementation ────────────────────────────────────────────────────

#[async_trait]
impl Tool for ExaSearchTool {
    fn name(&self) -> &str {
        "ExaSearch"
    }

    fn description(&self) -> &str {
        "AI-powered web search using Exa (https://exa.ai). Returns structured results with \
         optional text content, highlights, and summaries. Requires EXA_API_KEY environment variable."
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
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "search_type": {
                    "type": "string",
                    "description": "Search method: auto, neural, or fast (default: auto)",
                    "enum": ["auto", "neural", "fast"]
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (default 10, max 100)"
                },
                "category": {
                    "type": "string",
                    "description": "Focus category for results",
                    "enum": ["company", "research paper", "news", "personal site", "financial report", "people"]
                },
                "content_mode": {
                    "type": "string",
                    "description": "Content to retrieve: text, highlights, summary, or all (default: highlights)",
                    "enum": ["text", "highlights", "summary", "all"]
                },
                "max_characters": {
                    "type": "integer",
                    "description": "Max characters for text/highlight content per result"
                },
                "include_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only include results from these domains"
                },
                "exclude_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exclude results from these domains"
                },
                "start_published_date": {
                    "type": "string",
                    "description": "Earliest publication date (ISO 8601, e.g. 2024-01-01T00:00:00.000Z)"
                },
                "end_published_date": {
                    "type": "string",
                    "description": "Latest publication date (ISO 8601, e.g. 2024-12-31T23:59:59.000Z)"
                },
                "user_location": {
                    "type": "string",
                    "description": "Two-letter ISO country code for location bias (e.g. US, GB)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let api_key = match std::env::var(EXA_API_KEY_ENV) {
            Ok(k) if !k.is_empty() => k,
            _ => {
                return ToolResult::error(format!(
                    "Exa search requires {}. Get a key at https://dashboard.exa.ai/api-keys",
                    EXA_API_KEY_ENV
                ))
            }
        };

        let num_results = input.num_results.unwrap_or(10).min(100);
        let content_mode = input.content_mode.as_deref().unwrap_or("highlights");

        let contents = build_contents(content_mode, input.max_characters);

        let request_body = ExaSearchRequest {
            query: input.query.clone(),
            r#type: input.search_type.or_else(|| Some("auto".to_string())),
            num_results: Some(num_results),
            category: input.category,
            contents: Some(contents),
            include_domains: input.include_domains,
            exclude_domains: input.exclude_domains,
            start_published_date: input.start_published_date,
            end_published_date: input.end_published_date,
            user_location: input.user_location,
        };

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("HTTP client error: {}", e)),
        };

        let response = match client
            .post(EXA_SEARCH_URL)
            .header("x-api-key", &api_key)
            .header("x-exa-integration", "cersei")
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Exa search request failed: {}", e)),
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return ToolResult::error(format!("Exa API error ({}): {}", status, body));
        }

        let exa_response: ExaSearchResponse = match response.json().await {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Failed to parse Exa response: {}", e)),
        };

        let output = format_results(&exa_response.results, num_results);

        if output.is_empty() {
            ToolResult::success(format!("No results found for: {}", input.query))
        } else {
            ToolResult::success(output)
        }
    }
}

/// Build the `contents` object based on the requested content mode.
fn build_contents(mode: &str, max_characters: Option<usize>) -> ExaContents {
    match mode {
        "text" => ExaContents {
            text: Some(ExaTextOptions { max_characters }),
            highlights: None,
            summary: None,
        },
        "highlights" => ExaContents {
            text: None,
            highlights: Some(ExaHighlightsOptions {
                max_characters,
                query: None,
            }),
            summary: None,
        },
        "summary" => ExaContents {
            text: None,
            highlights: None,
            summary: Some(ExaSummaryOptions { query: None }),
        },
        // "all" — request text, highlights, and summary together
        _ => ExaContents {
            text: Some(ExaTextOptions { max_characters }),
            highlights: Some(ExaHighlightsOptions {
                max_characters,
                query: None,
            }),
            summary: Some(ExaSummaryOptions { query: None }),
        },
    }
}

/// Format search results into readable markdown text.
fn format_results(results: &[ExaResult], limit: usize) -> String {
    let mut output = String::new();
    for (i, result) in results.iter().enumerate().take(limit) {
        let title = result.title.as_deref().unwrap_or("(no title)");
        output.push_str(&format!("{}. **{}**\n", i + 1, title));
        output.push_str(&format!("   {}\n", result.url));

        if let Some(author) = &result.author {
            if !author.is_empty() {
                output.push_str(&format!("   Author: {}\n", author));
            }
        }
        if let Some(date) = &result.published_date {
            if !date.is_empty() {
                output.push_str(&format!("   Published: {}\n", date));
            }
        }

        // Content: cascade through summary -> highlights -> text
        let snippet = extract_snippet(result);
        if !snippet.is_empty() {
            output.push_str(&format!("   {}\n", snippet));
        }

        output.push('\n');
    }
    output
}

/// Extract the best available snippet from a result, cascading through
/// summary, highlights, and text fields.
fn extract_snippet(result: &ExaResult) -> String {
    if let Some(summary) = &result.summary {
        if !summary.is_empty() {
            return summary.clone();
        }
    }
    if let Some(highlights) = &result.highlights {
        let joined = highlights.join(" ... ");
        if !joined.is_empty() {
            return joined;
        }
    }
    if let Some(text) = &result.text {
        if !text.is_empty() {
            // Truncate long text to a reasonable snippet length
            let max_snippet = 500;
            if text.len() > max_snippet {
                return format!("{}...", &text[..max_snippet]);
            }
            return text.clone();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = ExaSearchTool;
        assert!(tool.input_schema()["properties"]["query"].is_object());
        assert_eq!(tool.category(), ToolCategory::Web);
        assert_eq!(tool.permission_level(), PermissionLevel::ReadOnly);
        assert_eq!(tool.name(), "ExaSearch");
    }

    #[test]
    fn test_parse_response() {
        let json = serde_json::json!({
            "requestId": "test-123",
            "results": [
                {
                    "title": "Rust Programming Language",
                    "url": "https://www.rust-lang.org",
                    "publishedDate": "2024-01-15",
                    "author": "Rust Team",
                    "text": "Rust is a systems programming language focused on safety.",
                    "highlights": ["Rust is focused on safety", "zero-cost abstractions"],
                    "summary": "Overview of the Rust programming language."
                },
                {
                    "title": "Learn Rust",
                    "url": "https://doc.rust-lang.org/book/",
                    "publishedDate": null,
                    "author": null,
                    "text": null,
                    "highlights": null,
                    "summary": null
                }
            ]
        });

        let response: ExaSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(response.results.len(), 2);

        let first = &response.results[0];
        assert_eq!(first.title.as_deref(), Some("Rust Programming Language"));
        assert_eq!(first.url, "https://www.rust-lang.org");
        assert_eq!(first.author.as_deref(), Some("Rust Team"));
        assert!(first.highlights.is_some());
        assert_eq!(first.highlights.as_ref().unwrap().len(), 2);
        assert_eq!(
            first.summary.as_deref(),
            Some("Overview of the Rust programming language.")
        );

        // Second result has all optional fields as None
        let second = &response.results[1];
        assert_eq!(second.title.as_deref(), Some("Learn Rust"));
        assert!(second.text.is_none());
        assert!(second.highlights.is_none());
        assert!(second.summary.is_none());
    }

    #[test]
    fn test_snippet_fallback_summary_first() {
        let result = ExaResult {
            title: Some("Test".into()),
            url: "https://example.com".into(),
            published_date: None,
            author: None,
            text: Some("Full text here".into()),
            highlights: Some(vec!["A highlight".into()]),
            summary: Some("A summary".into()),
        };
        assert_eq!(extract_snippet(&result), "A summary");
    }

    #[test]
    fn test_snippet_fallback_highlights_second() {
        let result = ExaResult {
            title: Some("Test".into()),
            url: "https://example.com".into(),
            published_date: None,
            author: None,
            text: Some("Full text here".into()),
            highlights: Some(vec!["First highlight".into(), "Second highlight".into()]),
            summary: None,
        };
        assert_eq!(
            extract_snippet(&result),
            "First highlight ... Second highlight"
        );
    }

    #[test]
    fn test_snippet_fallback_text_last() {
        let result = ExaResult {
            title: Some("Test".into()),
            url: "https://example.com".into(),
            published_date: None,
            author: None,
            text: Some("Only text available".into()),
            highlights: None,
            summary: None,
        };
        assert_eq!(extract_snippet(&result), "Only text available");
    }

    #[test]
    fn test_snippet_empty_when_nothing() {
        let result = ExaResult {
            title: Some("Test".into()),
            url: "https://example.com".into(),
            published_date: None,
            author: None,
            text: None,
            highlights: None,
            summary: None,
        };
        assert_eq!(extract_snippet(&result), "");
    }

    #[test]
    fn test_snippet_text_truncation() {
        let long_text = "a".repeat(600);
        let result = ExaResult {
            title: Some("Test".into()),
            url: "https://example.com".into(),
            published_date: None,
            author: None,
            text: Some(long_text),
            highlights: None,
            summary: None,
        };
        let snippet = extract_snippet(&result);
        assert!(snippet.ends_with("..."));
        assert_eq!(snippet.len(), 503); // 500 chars + "..."
    }

    #[test]
    fn test_build_contents_text_mode() {
        let contents = build_contents("text", Some(1000));
        assert!(contents.text.is_some());
        assert!(contents.highlights.is_none());
        assert!(contents.summary.is_none());
        assert_eq!(contents.text.unwrap().max_characters, Some(1000));
    }

    #[test]
    fn test_build_contents_highlights_mode() {
        let contents = build_contents("highlights", None);
        assert!(contents.text.is_none());
        assert!(contents.highlights.is_some());
        assert!(contents.summary.is_none());
    }

    #[test]
    fn test_build_contents_summary_mode() {
        let contents = build_contents("summary", None);
        assert!(contents.text.is_none());
        assert!(contents.highlights.is_none());
        assert!(contents.summary.is_some());
    }

    #[test]
    fn test_build_contents_all_mode() {
        let contents = build_contents("all", Some(500));
        assert!(contents.text.is_some());
        assert!(contents.highlights.is_some());
        assert!(contents.summary.is_some());
    }

    #[test]
    fn test_format_results_empty() {
        let results: Vec<ExaResult> = vec![];
        assert_eq!(format_results(&results, 10), "");
    }

    #[test]
    fn test_format_results_with_metadata() {
        let results = vec![ExaResult {
            title: Some("Test Page".into()),
            url: "https://example.com".into(),
            published_date: Some("2024-06-01".into()),
            author: Some("Jane Doe".into()),
            text: None,
            highlights: Some(vec!["key insight".into()]),
            summary: None,
        }];
        let output = format_results(&results, 10);
        assert!(output.contains("**Test Page**"));
        assert!(output.contains("https://example.com"));
        assert!(output.contains("Author: Jane Doe"));
        assert!(output.contains("Published: 2024-06-01"));
        assert!(output.contains("key insight"));
    }

    #[tokio::test]
    async fn test_disabled_without_api_key() {
        // Ensure the env var is unset for this test
        std::env::remove_var(EXA_API_KEY_ENV);

        let tool = ExaSearchTool;
        let ctx = ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            permissions: std::sync::Arc::new(
                crate::permissions::AllowAll,
            ),
            cost_tracker: std::sync::Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        };

        let result = tool
            .execute(
                serde_json::json!({"query": "test"}),
                &ctx,
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("EXA_API_KEY"));
    }
}
