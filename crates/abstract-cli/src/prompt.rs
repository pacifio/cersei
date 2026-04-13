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
    model: &str,
) -> String {
    let memory_content = memory_manager.build_context();

    // Git snapshot (computed once, used for both environment block and prompt injection)
    let git_status = build_git_snapshot(&config.working_dir);

    // Environment info (dynamic)
    let now = chrono::Local::now();
    let extra_dynamic = vec![(
        "environment".to_string(),
        format!(
            "Model: {}\nPlatform: {} {}\nShell: {}\nWorking directory: {}\nGit repo: {}\nDate: {}",
            model,
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::env::var("SHELL").unwrap_or_else(|_| "unknown".into()),
            config.working_dir.display(),
            if git_status.is_some() { "yes" } else { "no" },
            now.format("%Y-%m-%d %H:%M %Z"),
        ),
    )];

    // Project instructions: walk up directory tree for AGENTS.md, CLAUDE.md, CONTEXT.md
    let mut extra_cached: Vec<(String, String)> = Vec::new();
    let instruction_files = collect_instruction_files(&config.working_dir);
    for (path_label, content) in instruction_files {
        extra_cached.push(("project_instructions".to_string(), format!("# From: {}\n{}", path_label, content)));
    }

    // Tree-sitter project intelligence: scan source files for imports + symbols,
    // rank by importance (entry points, most-imported, most symbols), and inject
    // a compact summary. This gives the model a dependency graph to guide exploration
    // without giving it the full content (so it still needs to Read files).
    let project_intel = cersei_tools::tool_primitives::code_intel::scan_project(
        &config.working_dir,
        20, // top 20 most important files
    );
    if !project_intel.is_empty() {
        let intel_summary = cersei_tools::tool_primitives::code_intel::format_project_intel(&project_intel);
        extra_cached.push((
            "project_intel".to_string(),
            format!(
                "Project structure (top {} files by importance — symbols and imports extracted via tree-sitter):\n{}",
                project_intel.len(),
                intel_summary
            ),
        ));
    }

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

/// Walk up from working_dir collecting instruction files (AGENTS.md, CLAUDE.md, etc.).
/// Returns files in outermost-first order (project root instructions come first).
fn collect_instruction_files(working_dir: &std::path::Path) -> Vec<(String, String)> {
    use std::path::Path;

    const INSTRUCTION_FILES: &[&str] = &[
        "AGENTS.md",
        "CLAUDE.md",
        "CONTEXT.md",
        ".abstract/instructions.md",
    ];

    let mut found: Vec<(String, String)> = Vec::new();
    let mut current = working_dir.to_path_buf();

    loop {
        for filename in INSTRUCTION_FILES {
            let path = current.join(filename);
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if !content.trim().is_empty() {
                        let label = path.strip_prefix(working_dir)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| path.display().to_string());
                        found.push((label, content));
                    }
                }
            }
        }

        if !current.pop() {
            break;
        }
    }

    // Reverse so outermost (root-level) files come first
    found.reverse();
    found
}

/// Build a file tree for project awareness (first N files).
/// Uses `git ls-files` if in a git repo, otherwise walkdir with exclusions.
fn build_file_tree(working_dir: &std::path::Path, max_files: usize) -> Option<String> {
    use std::process::Command;

    // Try git ls-files first (fast, respects .gitignore)
    let git_output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(working_dir)
        .output()
        .ok();

    if let Some(output) = git_output {
        if output.status.success() {
            let files: String = String::from_utf8_lossy(&output.stdout)
                .lines()
                .take(max_files)
                .collect::<Vec<_>>()
                .join("\n");
            if !files.is_empty() {
                let total = String::from_utf8_lossy(&output.stdout).lines().count();
                let mut result = files;
                if total > max_files {
                    result.push_str(&format!("\n\n({total} files total, showing first {max_files})"));
                }
                return Some(result);
            }
        }
    }

    // Fallback: walkdir with exclusions
    let excluded = ["node_modules", "target", ".git", "__pycache__", ".venv", "venv", "dist", "build", ".next"];
    let mut files = Vec::new();

    fn walk(dir: &std::path::Path, base: &std::path::Path, excluded: &[&str], files: &mut Vec<String>, max: usize) {
        if files.len() >= max {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if files.len() >= max {
                return;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || excluded.contains(&name.as_str()) {
                continue;
            }
            if path.is_file() {
                if let Ok(rel) = path.strip_prefix(base) {
                    files.push(rel.display().to_string());
                }
            } else if path.is_dir() {
                walk(&path, base, excluded, files, max);
            }
        }
    }

    walk(working_dir, working_dir, &excluded, &mut files, max_files);
    files.sort();

    if files.is_empty() {
        None
    } else {
        Some(files.join("\n"))
    }
}
