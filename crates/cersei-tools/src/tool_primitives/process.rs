//! Async command execution primitives.
//!
//! Stateless — no persistent cwd or env. Each call is independent.
//! Shell state persistence (for coding agents) is a higher-level concern.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// Result of executing a command.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

/// Options for command execution.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub timeout: Option<Duration>,
    pub shell: Shell,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            env: HashMap::new(),
            timeout: Some(Duration::from_secs(120)),
            shell: Shell::Sh,
        }
    }
}

/// Which shell to use for command execution.
#[derive(Debug, Clone)]
pub enum Shell {
    Sh,
    Bash,
    Zsh,
    PowerShell,
    Cmd,
    Custom { program: String, args: Vec<String> },
}

/// A line of output from a streaming command.
#[derive(Debug, Clone)]
pub enum OutputLine {
    Stdout(String),
    Stderr(String),
}

/// Execute a command through a shell. Returns when the command completes or times out.
pub async fn exec(command: &str, opts: ExecOptions) -> Result<ExecOutput, std::io::Error> {
    let (program, args) = shell_args(&opts.shell, command);

    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }

    for (k, v) in &opts.env {
        cmd.env(k, v);
    }

    let child = cmd.spawn()?;

    let timeout = opts.timeout.unwrap_or(Duration::from_secs(120));

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            timed_out: false,
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            // Timeout — process is dropped (killed automatically)
            Ok(ExecOutput {
                stdout: String::new(),
                stderr: format!("Command timed out after {}s", timeout.as_secs()),
                exit_code: -1,
                timed_out: true,
            })
        }
    }
}

/// Execute a command and stream output lines through a channel.
///
/// Returns a receiver for output lines and a join handle that resolves
/// to the final `ExecOutput` when the command completes.
pub fn exec_streaming(
    command: &str,
    opts: ExecOptions,
) -> Result<
    (
        mpsc::Receiver<OutputLine>,
        tokio::task::JoinHandle<ExecOutput>,
    ),
    std::io::Error,
> {
    let (program, args) = shell_args(&opts.shell, command);

    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }

    for (k, v) in &opts.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()?;
    let (tx, rx) = mpsc::channel(256);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let timeout = opts.timeout.unwrap_or(Duration::from_secs(120));

    let handle = tokio::spawn(async move {
        let mut full_stdout = String::new();
        let mut full_stderr = String::new();

        let tx_out = tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut collected = String::new();
            if let Some(stdout) = stdout {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    collected.push_str(&line);
                    collected.push('\n');
                    let _ = tx_out.send(OutputLine::Stdout(line)).await;
                }
            }
            collected
        });

        let tx_err = tx;
        let stderr_task = tokio::spawn(async move {
            let mut collected = String::new();
            if let Some(stderr) = stderr {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    collected.push_str(&line);
                    collected.push('\n');
                    let _ = tx_err.send(OutputLine::Stderr(line)).await;
                }
            }
            collected
        });

        let result = tokio::time::timeout(timeout, child.wait()).await;

        full_stdout = stdout_task.await.unwrap_or_default();
        full_stderr = stderr_task.await.unwrap_or_default();

        match result {
            Ok(Ok(status)) => ExecOutput {
                stdout: full_stdout,
                stderr: full_stderr,
                exit_code: status.code().unwrap_or(-1),
                timed_out: false,
            },
            _ => {
                let _ = child.kill().await;
                ExecOutput {
                    stdout: full_stdout,
                    stderr: full_stderr,
                    exit_code: -1,
                    timed_out: true,
                }
            }
        }
    });

    Ok((rx, handle))
}

fn shell_args(shell: &Shell, command: &str) -> (String, Vec<String>) {
    match shell {
        Shell::Sh => ("sh".into(), vec!["-c".into(), command.into()]),
        Shell::Bash => ("bash".into(), vec!["-c".into(), command.into()]),
        Shell::Zsh => ("zsh".into(), vec!["-c".into(), command.into()]),
        Shell::PowerShell => (
            "pwsh".into(),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                command.into(),
            ],
        ),
        Shell::Cmd => ("cmd".into(), vec!["/C".into(), command.into()]),
        Shell::Custom { program, args } => {
            let mut a = args.clone();
            a.push(command.into());
            (program.clone(), a)
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_exec_echo() {
        let out = exec("echo hello", ExecOptions::default()).await.unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.trim() == "hello");
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn test_exec_exit_code() {
        let out = exec("exit 42", ExecOptions::default()).await.unwrap();
        assert_eq!(out.exit_code, 42);
    }

    #[tokio::test]
    async fn test_exec_with_cwd() {
        let out = exec(
            "pwd",
            ExecOptions {
                cwd: Some("/tmp".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(out.stdout.contains("tmp"));
    }

    #[tokio::test]
    async fn test_exec_with_env() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".into(), "hello_world".into());
        let out = exec(
            "echo $MY_VAR",
            ExecOptions {
                env,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(out.stdout.contains("hello_world"));
    }

    #[tokio::test]
    async fn test_exec_timeout() {
        let out = exec(
            "sleep 10",
            ExecOptions {
                timeout: Some(Duration::from_millis(100)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(out.timed_out);
    }
}
