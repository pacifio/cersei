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
    if config.benchmark_mode {
        return build_benchmark_prompt(model, &config.working_dir);
    }
    // F-A8: only the MEMORY.md index — which can change mid-session as
    // memories are stored — stays on the dynamic side of the cache boundary.
    // The CLAUDE.md hierarchy is stable within a session and is injected on
    // the cacheable side below.
    let memory_content = memory_manager.build_memory_index();

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

    // Project instructions, all on the cacheable side (F-A8).
    // The CLAUDE.md hierarchy (managed rules, user, project, local) comes from
    // the memory manager — with frontmatter stripping and @include expansion —
    // and the directory walk below skips any file the hierarchy already
    // loaded, so {root}/CLAUDE.md is injected exactly once.
    let mut extra_cached: Vec<(String, String)> = Vec::new();
    let mut already_loaded: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    for file in memory_manager.claude_instruction_files() {
        already_loaded.insert(file.path.canonicalize().unwrap_or_else(|_| file.path.clone()));
        extra_cached.push((
            "project_instructions".to_string(),
            format!("# From: {}\n{}", file.path.display(), file.content),
        ));
    }

    // Walk up the directory tree for AGENTS.md, CONTEXT.md, and any
    // ancestor CLAUDE.md the hierarchy does not cover.
    let instruction_files = collect_instruction_files(&config.working_dir, &already_loaded);
    for (path_label, content) in instruction_files {
        extra_cached.push((
            "project_instructions".to_string(),
            format!("# From: {}\n{}", path_label, content),
        ));
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
        let intel_summary =
            cersei_tools::tool_primitives::code_intel::format_project_intel(&project_intel);
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
        .map(|s| {
            s.lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let recent_commits: Vec<String> = Command::new("git")
        .args(["log", "--oneline", "-5"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| {
            s.lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
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
/// Paths in `skip` (canonicalized) were already injected by another loader
/// and are not read again (F-A8).
fn collect_instruction_files(
    working_dir: &std::path::Path,
    skip: &std::collections::HashSet<std::path::PathBuf>,
) -> Vec<(String, String)> {
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
            if skip.contains(&path.canonicalize().unwrap_or_else(|_| path.clone())) {
                continue;
            }
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if !content.trim().is_empty() {
                        let label = path
                            .strip_prefix(working_dir)
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
                    result.push_str(&format!(
                        "\n\n({total} files total, showing first {max_files})"
                    ));
                }
                return Some(result);
            }
        }
    }

    // Fallback: walkdir with exclusions
    let excluded = [
        "node_modules",
        "target",
        ".git",
        "__pycache__",
        ".venv",
        "venv",
        "dist",
        "build",
        ".next",
    ];
    let mut files = Vec::new();

    fn walk(
        dir: &std::path::Path,
        base: &std::path::Path,
        excluded: &[&str],
        files: &mut Vec<String>,
        max: usize,
    ) {
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

/// Benchmark-optimized system prompt for terminal-bench 2.0.
/// Focus on solving the task — tests are run externally by the verifier.
fn build_benchmark_prompt(model: &str, working_dir: &std::path::Path) -> String {
    let wd = working_dir.display();
    let mut prompt = format!(
        r#"You are a coding agent inside a Docker container. Your ONLY job is to complete the task correctly. NEVER explain or narrate — only run commands and write code.

Model: {model}
Working directory: {wd}

## PHASE 1: RECON (always do this first, ALL calls in parallel)
In your FIRST response, make ALL of these tool calls IN PARALLEL:
1. Bash: `ls -laR {wd}/ 2>/dev/null | head -60`
2. Bash: `find {wd} -type f -name "*.py" -o -name "*.sh" -o -name "*.c" -o -name "*.rs" -o -name "*.js" -o -name "*.toml" -o -name "*.yaml" -o -name "*.json" -o -name "Makefile" 2>/dev/null | head -30 | xargs cat 2>/dev/null | head -300`
3. Bash: `cat {wd}/README* {wd}/*.md {wd}/*.txt 2>/dev/null | head -150`

## PHASE 2: PLAN (mandatory before coding)
After reading the files, make a mental plan:
- What EXACTLY does the task require? What files/outputs must exist?
- What existing code/data is already provided? What must you build?
- If the instruction mentions a test or verification command, note it.
- What's the simplest approach that could work?

## PHASE 3: IMPLEMENT
- Write the SIMPLEST solution that satisfies ALL task requirements.
- ALWAYS read existing files completely before modifying them.
- Use parallel tool calls when operations are independent.
- Install dependencies with `pip install` or `apt-get` as needed.

## PHASE 4: VERIFY
- If the instruction mentions a verification command (e.g. "run test_outputs.py"), run it NOW.
- If no test command: verify by running your solution and checking the output yourself.
- Check that ALL expected output files exist with correct content.
- Re-read the original instruction one more time — did you miss anything?

## PHASE 5: ERROR RECOVERY (if something fails)
When a command or test fails, you MUST:
1. Read the FULL error output — errors are often at the end.
2. Identify the ROOT CAUSE — what specifically went wrong?
3. Think about WHY it happened — wrong assumption? Missing dependency? Wrong format?
4. Fix with a TARGETED change if the approach is sound, OR try a COMPLETELY DIFFERENT approach if the logic is wrong.
Do NOT blindly retry the same command. Do NOT skip this reflection.

## RULES
- NEVER explain. NEVER narrate. NEVER ask questions. Only code and commands.
- Use parallel tool calls whenever operations are independent.
- If output is too long, use `| tail -50` or `| head -50` to see relevant parts.
- If stuck: simplify. The simplest interpretation of the task is usually correct.
- Speed matters. Don't over-engineer.
- If installing packages, prefer `pip install` over building from source.
- For long-running operations (training, compilation), monitor progress with periodic checks.
- Do NOT look for or try to run /tests/run-tests.sh — tests are run externally after you finish.
- Focus all effort on producing correct output in {wd}.
- ALWAYS verify your solution works before finishing.
"#
    );

    // Append learned failure patterns if available
    if let Ok(patterns) = std::env::var("ABSTRACT_FAILURE_PATTERNS") {
        if !patterns.is_empty() {
            prompt.push_str("\n## LEARNED PATTERNS (from previous runs — avoid these mistakes)\n");
            prompt.push_str(&patterns);
            prompt.push('\n');
        }
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use cersei_agent::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;

    /// F-A8: {root}/CLAUDE.md must appear exactly once in the assembled
    /// prompt, on the cacheable side of the dynamic boundary. Before the fix
    /// it was injected twice — raw via the instruction-file walk (cached) and
    /// again via `MemoryManager::build_context` (dynamic).
    #[test]
    fn claude_md_injected_once_on_the_cached_side() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_sentinel = "F_A8_CLAUDE_MD_SENTINEL_7c1d";
        let agents_sentinel = "F_A8_AGENTS_MD_SENTINEL_7c1d";
        std::fs::write(
            tmp.path().join("CLAUDE.md"),
            format!("# Rules\n{claude_sentinel}"),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("AGENTS.md"),
            format!("# Agents\n{agents_sentinel}"),
        )
        .unwrap();

        let config = AppConfig {
            working_dir: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let memory_manager =
            MemoryManager::new(&config.working_dir).with_memory_dir(tmp.path().join("mem"));

        let prompt = build_cli_system_prompt(&config, &memory_manager, "claude-fable-5");

        let boundary = prompt
            .find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            .expect("prompt must contain the dynamic boundary");
        assert_eq!(
            prompt.matches(claude_sentinel).count(),
            1,
            "CLAUDE.md must be injected exactly once"
        );
        assert!(
            prompt.find(claude_sentinel).unwrap() < boundary,
            "CLAUDE.md must sit on the cacheable side of the boundary"
        );
        // The instruction-file walk still collects non-CLAUDE.md files, cached.
        assert_eq!(prompt.matches(agents_sentinel).count(), 1);
        assert!(prompt.find(agents_sentinel).unwrap() < boundary);
    }
}
