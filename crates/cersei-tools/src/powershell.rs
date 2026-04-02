//! PowerShell tool: execute PowerShell commands (Windows/cross-platform).

use super::*;
use serde::Deserialize;
use std::process::Stdio;

pub struct PowerShellTool;

#[async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str { "PowerShell" }
    fn description(&self) -> &str { "Execute a PowerShell command. Available on Windows; on macOS/Linux uses pwsh if installed." }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::Execute }
    fn category(&self) -> ToolCategory { ToolCategory::Shell }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "PowerShell command to execute" },
                "timeout": { "type": "integer", "description": "Timeout in milliseconds (default 120000)" }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input { command: String, timeout: Option<u64> }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let ps = if cfg!(windows) { "powershell" } else { "pwsh" };
        if !cfg!(windows) && which::which(ps).is_err() {
            return ToolResult::error("PowerShell (pwsh) not found. Install with: brew install powershell");
        }

        let timeout_ms = input.timeout.unwrap_or(120_000).min(600_000);

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            tokio::process::Command::new(ps)
                .args(["-NoProfile", "-NonInteractive", "-Command", &input.command])
                .current_dir(&ctx.working_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut content = String::new();
                if !stdout.is_empty() { content.push_str(&stdout); }
                if !stderr.is_empty() {
                    if !content.is_empty() { content.push('\n'); }
                    content.push_str(&stderr);
                }
                if output.status.success() {
                    ToolResult::success(if content.is_empty() { "(no output)".into() } else { content })
                } else {
                    ToolResult::error(format!("Exit code {}\n{}", output.status.code().unwrap_or(-1), content))
                }
            }
            Ok(Err(e)) => ToolResult::error(format!("Failed to execute: {}", e)),
            Err(_) => ToolResult::error(format!("Timed out after {}ms", timeout_ms)),
        }
    }
}
