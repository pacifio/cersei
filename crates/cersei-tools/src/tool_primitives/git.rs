//! Async git operations via the git CLI.
//!
//! All functions use `tokio::process::Command` — no blocking I/O.

use std::path::{Path, PathBuf};

/// Git repository status.
#[derive(Debug, Clone)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub files: Vec<GitFileStatus>,
}

/// Status of a single file.
#[derive(Debug, Clone)]
pub struct GitFileStatus {
    pub path: String,
    pub status: String, // "M", "A", "D", "??", etc.
}

/// A single git log entry.
#[derive(Debug, Clone)]
pub struct GitLogEntry {
    pub hash: String,
    pub message: String,
}

/// Git errors.
#[derive(Debug)]
pub enum GitError {
    NotARepo,
    CommandFailed(String),
    IoError(std::io::Error),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARepo => write!(f, "not a git repository"),
            Self::CommandFailed(msg) => write!(f, "git command failed: {msg}"),
            Self::IoError(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for GitError {}

impl From<std::io::Error> for GitError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

async fn git_cmd(path: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(GitError::CommandFailed(stderr))
    }
}

/// Check if a path is inside a git repository.
pub async fn is_repo(path: &Path) -> bool {
    git_cmd(path, &["rev-parse", "--is-inside-work-tree"])
        .await
        .map(|s| s == "true")
        .unwrap_or(false)
}

/// Get the repository root directory.
pub async fn repo_root(path: &Path) -> Option<PathBuf> {
    git_cmd(path, &["rev-parse", "--show-toplevel"])
        .await
        .ok()
        .map(PathBuf::from)
}

/// Get the current branch name.
pub async fn current_branch(path: &Path) -> Option<String> {
    git_cmd(path, &["branch", "--show-current"])
        .await
        .ok()
        .filter(|s| !s.is_empty())
}

/// Get the repository status (branch + changed files).
pub async fn status(path: &Path) -> Result<GitStatus, GitError> {
    let branch = current_branch(path).await;

    let output = git_cmd(path, &["status", "--porcelain"]).await?;
    let files: Vec<GitFileStatus> = output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let status = line[..2].trim().to_string();
            let file_path = line[3..].to_string();
            GitFileStatus {
                path: file_path,
                status,
            }
        })
        .collect();

    Ok(GitStatus { branch, files })
}

/// Get the diff (unified format).
/// `staged = true` shows staged changes, `false` shows unstaged.
pub async fn diff(path: &Path, staged: bool) -> Result<String, GitError> {
    if staged {
        git_cmd(path, &["diff", "--cached"])
    } else {
        git_cmd(path, &["diff"])
    }
    .await
}

/// Get the diff for a specific file.
pub async fn diff_file_content(path: &Path, file: &str) -> Result<String, GitError> {
    git_cmd(path, &["diff", "--", file]).await
}

/// Get recent log entries.
pub async fn log(path: &Path, n: usize) -> Result<Vec<GitLogEntry>, GitError> {
    let output = git_cmd(path, &["log", "--oneline", &format!("-{n}")]).await?;
    let entries = output
        .lines()
        .filter_map(|line| {
            let (hash, message) = line.split_once(' ')?;
            Some(GitLogEntry {
                hash: hash.to_string(),
                message: message.to_string(),
            })
        })
        .collect();
    Ok(entries)
}

/// List files modified since HEAD.
pub async fn list_modified_files(path: &Path) -> Result<Vec<String>, GitError> {
    let output = git_cmd(path, &["diff", "--name-only", "HEAD"]).await?;
    Ok(output.lines().map(String::from).filter(|s| !s.is_empty()).collect())
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_is_repo_false() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_repo(tmp.path()).await);
    }

    #[tokio::test]
    async fn test_is_repo_true() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .await
            .unwrap();
        assert!(is_repo(tmp.path()).await);
    }

    #[tokio::test]
    async fn test_repo_root() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .await
            .unwrap();
        let root = repo_root(tmp.path()).await;
        assert!(root.is_some());
    }

    #[tokio::test]
    async fn test_status_empty_repo() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .await
            .unwrap();

        let st = status(tmp.path()).await.unwrap();
        assert!(st.files.is_empty());
    }
}
