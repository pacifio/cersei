//! Grep tool: search file contents with regex.

use super::*;
use serde::Deserialize;
use std::process::Stdio;

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

        // Use ripgrep if available, fall back to grep
        let rg_available = which::which("rg").is_ok();

        let mut cmd = if rg_available {
            let mut c = tokio::process::Command::new("rg");
            c.args(["--no-heading", "-n", &input.pattern, &search_path]);
            if let Some(g) = &input.glob {
                c.args(["--glob", g]);
            }
            c.args(["--max-count", "250"]);
            c
        } else {
            let mut c = tokio::process::Command::new("grep");
            c.args(["-rn", &input.pattern, &search_path]);
            c.args(["--max-count=250"]);
            c
        };

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        match cmd.output().await {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim().is_empty() {
                    ToolResult::success("No matches found.")
                } else {
                    ToolResult::success(stdout.to_string())
                }
            }
            Err(e) => ToolResult::error(format!("Search failed: {}", e)),
        }
    }
}
