//! Bash tool: execute shell commands with persistent shell state.
//!
//! Uses sentinel markers to capture pwd after each command execution,
//! persisting the working directory across calls.

use super::*;
use crate::tool_primitives::process::{self as pproc, ExecOptions, Shell};
use serde::Deserialize;

/// Parse stdout to separate user output from sentinel-captured state.
/// Returns (user_visible_output, Option<new_cwd>).
fn parse_sentinel_output(stdout: &str, sentinel: &str) -> (String, Option<String>) {
    if let Some(pos) = stdout.rfind(sentinel) {
        let user_output = stdout[..pos].trim_end_matches('\n').to_string();
        let state_section = &stdout[pos + sentinel.len()..];
        let new_cwd = state_section
            .trim()
            .lines()
            .next()
            .map(|s| s.trim().to_string());
        (user_output, new_cwd)
    } else {
        // Sentinel not found (command may have failed before reaching it)
        (stdout.to_string(), None)
    }
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command and return its output. The working directory persists between commands."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }

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

        // Wrap command with sentinel-based state capture
        // After the user's command runs, we capture pwd to persist cwd
        const SENTINEL: &str = "__ABSTRACT_STATE_7f2a9b__";
        let wrapped_command = format!(
            "cd '{}' 2>/dev/null; {} ; __abstract_exit=$?; echo '{}'; pwd; exit $__abstract_exit",
            cwd.display(),
            input.command,
            SENTINEL,
        );

        let opts = ExecOptions {
            cwd: Some(ctx.working_dir.clone()), // base cwd, actual cd is in the script
            env: env_vars,
            timeout: Some(std::time::Duration::from_millis(timeout_ms)),
            shell: Shell::Sh,
        };

        match pproc::exec(&wrapped_command, opts).await {
            Ok(output) => {
                if output.timed_out {
                    return ToolResult::error(format!("Command timed out after {}ms", timeout_ms));
                }

                // Parse sentinel-based output to extract new cwd
                let (user_output, new_cwd) = parse_sentinel_output(&output.stdout, SENTINEL);

                // Persist new cwd
                if let Some(new_dir) = new_cwd {
                    let path = PathBuf::from(&new_dir);
                    if path.exists() {
                        shell_state.lock().cwd = Some(path);
                    }
                }

                let mut content = user_output;
                if !output.stderr.is_empty() {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(&output.stderr);
                }

                if output.exit_code == 0 {
                    if content.is_empty() {
                        ToolResult::success("(Bash completed with no output)")
                    } else {
                        ToolResult::success(content)
                    }
                } else {
                    ToolResult::error(format!("Exit code {}\n{}", output.exit_code, content))
                }
            }
            Err(e) => ToolResult::error(format!("Failed to execute: {}", e)),
        }
    }
}
