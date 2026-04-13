//! Git utilities: repo detection, status, diff, and history.
//!
//! Used by the system prompt builder to inject git context.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Check if a path is inside a git repository.
pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get the root of the git repository.
pub fn get_repo_root(path: &Path) -> Option<PathBuf> {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
            } else {
                None
            }
        })
}

/// Get current branch name.
pub fn current_branch(path: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(path)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
}

/// Get git status (short format).
pub fn git_status(path: &Path) -> Option<String> {
    Command::new("git")
        .args(["status", "--short"])
        .current_dir(path)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
}

/// Get git diff (staged + unstaged).
pub fn git_diff(path: &Path) -> Option<String> {
    let staged = Command::new("git")
        .args(["diff", "--cached", "--stat"])
        .current_dir(path)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let unstaged = Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(path)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let combined = format!("{}\n{}", staged, unstaged).trim().to_string();
    if combined.is_empty() { None } else { Some(combined) }
}

/// Get recent commit history (one-line format).
pub fn recent_commits(path: &Path, count: usize) -> Option<String> {
    Command::new("git")
        .args(["log", "--oneline", &format!("-{}", count)])
        .current_dir(path)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
}

/// List modified files (both staged and unstaged).
pub fn list_modified_files(path: &Path) -> Vec<String> {
    Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(path)
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Build a git context string for the system prompt.
pub fn build_git_context(working_dir: &Path) -> Option<String> {
    if !is_git_repo(working_dir) {
        return None;
    }

    let mut parts = Vec::new();

    if let Some(branch) = current_branch(working_dir) {
        parts.push(format!("Current branch: {}", branch));
    }

    if let Some(status) = git_status(working_dir) {
        if !status.is_empty() {
            parts.push(format!("Status:\n{}", status));
        } else {
            parts.push("Status: (clean)".to_string());
        }
    }

    if let Some(commits) = recent_commits(working_dir, 5) {
        parts.push(format!("Recent commits:\n{}", commits));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_git_repo() {
        // The project root should be a git repo
        let cwd = std::env::current_dir().unwrap();
        // This might not be true in all test environments
        let _ = is_git_repo(&cwd); // just verify it doesn't panic
    }

    #[test]
    fn test_not_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(tmp.path()));
    }

    #[test]
    fn test_build_git_context_non_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(build_git_context(tmp.path()).is_none());
    }

    #[test]
    fn test_git_context_real_repo() {
        // Try on the actual project repo
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        if is_git_repo(root) {
            let ctx = build_git_context(root);
            assert!(ctx.is_some());
            let ctx = ctx.unwrap();
            assert!(ctx.contains("branch") || ctx.contains("Status"));
        }
    }
}
