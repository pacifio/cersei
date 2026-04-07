//! System prompt assembly with CLI-specific context injection.

use crate::config::AppConfig;
use cersei_agent::system_prompt::{
    build_system_prompt, GitSnapshot, OutputStyle, SystemPromptOptions, SystemPromptPrefix,
};
use cersei_memory::manager::MemoryManager;

/// Build the complete system prompt for the CLI agent.
pub fn build_cli_system_prompt(
    config: &AppConfig,
    memory_manager: &MemoryManager,
) -> String {
    let memory_content = memory_manager.build_context();

    // Environment info (dynamic)
    let now = chrono::Local::now();
    let extra_dynamic = vec![(
        "environment".to_string(),
        format!(
            "Date: {}\nOS: {} {}\nShell: {}",
            now.format("%Y-%m-%d %H:%M %Z"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::env::var("SHELL").unwrap_or_else(|_| "unknown".into()),
        ),
    )];

    // Project instructions (.abstract/instructions.md)
    let instructions_path = config.working_dir.join(".abstract").join("instructions.md");
    let extra_cached = if instructions_path.exists() {
        std::fs::read_to_string(&instructions_path)
            .ok()
            .map(|content| vec![("project_instructions".to_string(), content)])
            .unwrap_or_default()
    } else {
        vec![]
    };

    // Git snapshot
    let git_status = build_git_snapshot(&config.working_dir);

    // Tool names (all 34 built-in tools)
    let tools_available: Vec<String> = cersei_tools::all()
        .iter()
        .map(|t| t.name().to_string())
        .collect();

    let opts = SystemPromptOptions {
        prefix: Some(SystemPromptPrefix::Interactive),
        output_style: OutputStyle::from_str(&config.output_style),
        working_directory: Some(config.working_dir.display().to_string()),
        memory_content,
        extra_cached_sections: extra_cached,
        extra_dynamic_sections: extra_dynamic,
        has_auto_compact: config.auto_compact,
        has_memory: config.graph_memory,
        tools_available,
        git_status,
        ..Default::default()
    };

    build_system_prompt(&opts)
}

fn build_git_snapshot(working_dir: &std::path::Path) -> Option<GitSnapshot> {
    use std::process::Command;

    // Check if we're in a git repo
    let check = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(working_dir)
        .output()
        .ok()?;

    if !check.status.success() {
        return None;
    }

    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "detached".into());

    let user = Command::new("git")
        .args(["config", "user.name"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let status_lines: Vec<String> = Command::new("git")
        .args(["status", "--short"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default();

    let recent_commits: Vec<String> = Command::new("git")
        .args(["log", "--oneline", "-5"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default();

    Some(GitSnapshot {
        branch,
        recent_commits,
        status_lines,
        user,
    })
}
