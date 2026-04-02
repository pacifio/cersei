//! Bash tool: execute shell commands.

use super::*;
use serde::Deserialize;
use std::process::Stdio;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "Bash" }

    fn description(&self) -> &str {
        "Execute a bash command and return its output. The working directory persists between commands."
    }

    fn permission_level(&self) -> PermissionLevel { PermissionLevel::Execute }
    fn category(&self) -> ToolCategory { ToolCategory::Shell }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds (max 600000)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            command: String,
            timeout: Option<u64>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let shell_state = session_shell_state(&ctx.session_id);
        let (cwd, env_vars) = {
            let state = shell_state.lock();
            (
                state.cwd.clone().unwrap_or_else(|| ctx.working_dir.clone()),
                state.env_vars.clone(),
            )
        };

        let timeout_ms = input.timeout.unwrap_or(120_000).min(600_000);

        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", &input.command])
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (k, v) in &env_vars {
            cmd.env(k, v);
        }

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            cmd.output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                // Update shell state for cd commands
                if input.command.trim().starts_with("cd ") {
                    let dir = input.command.trim().strip_prefix("cd ").unwrap().trim();
                    let new_cwd = if dir.starts_with('/') {
                        PathBuf::from(dir)
                    } else {
                        cwd.join(dir)
                    };
                    if new_cwd.exists() {
                        shell_state.lock().cwd = Some(new_cwd);
                    }
                }

                let mut content = String::new();
                if !stdout.is_empty() {
                    content.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(&stderr);
                }

                if output.status.success() {
                    if content.is_empty() {
                        ToolResult::success("(Bash completed with no output)")
                    } else {
                        ToolResult::success(content)
                    }
                } else {
                    let code = output.status.code().unwrap_or(-1);
                    ToolResult::error(format!(
                        "Exit code {}\n{}",
                        code,
                        content
                    ))
                }
            }
            Ok(Err(e)) => ToolResult::error(format!("Failed to execute: {}", e)),
            Err(_) => ToolResult::error(format!(
                "Command timed out after {}ms",
                timeout_ms
            )),
        }
    }
}
