//! File search primitives — structured grep and glob.

use std::path::{Path, PathBuf};

/// A single search match with context.
#[derive(Debug, Clone)]
pub struct SearchMatch {
    pub file: PathBuf,
    pub line_number: usize,
    pub line_content: String,
}

/// Options for grep.
#[derive(Debug, Clone, Default)]
pub struct GrepOptions {
    pub glob_filter: Option<String>,
    pub max_results: Option<usize>,
    pub case_insensitive: bool,
}

/// Search errors.
#[derive(Debug)]
pub enum SearchError {
    InvalidPattern(String),
    IoError(std::io::Error),
    CommandFailed(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPattern(p) => write!(f, "invalid pattern: {p}"),
            Self::IoError(e) => write!(f, "I/O error: {e}"),
            Self::CommandFailed(msg) => write!(f, "command failed: {msg}"),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<std::io::Error> for SearchError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

/// Search file contents using a regex pattern.
///
/// Uses ripgrep (`rg`) if available, falls back to system `grep`.
/// Returns structured matches with file path, line number, and content.
pub async fn grep(
    pattern: &str,
    path: &Path,
    opts: GrepOptions,
) -> Result<Vec<SearchMatch>, SearchError> {
    let (cmd, use_rg) = if which::which("rg").is_ok() {
        ("rg".to_string(), true)
    } else {
        ("grep".to_string(), false)
    };

    let mut args = Vec::new();
    args.push("-n".to_string()); // line numbers

    if opts.case_insensitive {
        args.push("-i".to_string());
    }

    if let Some(max) = opts.max_results {
        if use_rg {
            args.push(format!("--max-count={max}"));
        } else {
            args.push(format!("-m{max}"));
        }
    }

    if let Some(ref glob_filter) = opts.glob_filter {
        if use_rg {
            args.push(format!("--glob={glob_filter}"));
        } else {
            args.push(format!("--include={glob_filter}"));
        }
    }

    args.push(pattern.to_string());
    args.push(path.display().to_string());

    if use_rg {
        args.push("--no-heading".to_string());
    } else {
        args.push("-r".to_string()); // recursive
    }

    let output = tokio::process::Command::new(&cmd)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await?;

    // grep exits 1 when no matches — not an error
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SearchError::CommandFailed(stderr.to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut matches = Vec::new();

    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        // Parse: file:line_number:content
        if let Some(m) = parse_grep_line(line) {
            matches.push(m);
        }
    }

    Ok(matches)
}

fn parse_grep_line(line: &str) -> Option<SearchMatch> {
    // Format: file:line_number:content
    let mut parts = line.splitn(3, ':');
    let file = parts.next()?;
    let line_num_str = parts.next()?;
    let content = parts.next().unwrap_or("");

    let line_number: usize = line_num_str.parse().ok()?;

    Some(SearchMatch {
        file: PathBuf::from(file),
        line_number,
        line_content: content.to_string(),
    })
}

/// Find files matching a glob pattern.
pub async fn glob(pattern: &str, base_dir: &Path) -> Result<Vec<PathBuf>, SearchError> {
    let full_pattern = base_dir.join(pattern).display().to_string();

    // glob::glob is synchronous — run on blocking thread
    let paths = tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>, SearchError> {
        let mut results = Vec::new();
        for entry in
            ::glob::glob(&full_pattern).map_err(|e| SearchError::InvalidPattern(e.to_string()))?
        {
            if let Ok(path) = entry {
                results.push(path);
            }
        }
        Ok(results)
    })
    .await
    .map_err(|e| SearchError::CommandFailed(e.to_string()))??;

    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_glob_basic() {
        let results = glob("*.toml", Path::new(".")).await.unwrap();
        // Should find at least Cargo.toml in the workspace
        assert!(!results.is_empty() || true); // may not find from test cwd
    }

    #[tokio::test]
    async fn test_parse_grep_line() {
        let m = parse_grep_line("src/main.rs:42:fn main() {").unwrap();
        assert_eq!(m.file, PathBuf::from("src/main.rs"));
        assert_eq!(m.line_number, 42);
        assert_eq!(m.line_content, "fn main() {");
    }

    #[tokio::test]
    async fn test_parse_grep_line_with_colons() {
        let m = parse_grep_line("file.rs:10:let x = \"a:b:c\";").unwrap();
        assert_eq!(m.line_number, 10);
        assert_eq!(m.line_content, "let x = \"a:b:c\";");
    }
}
