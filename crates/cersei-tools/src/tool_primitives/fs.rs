//! Async file operation primitives.
//!
//! Read, write, edit, diff, and patch files. All async via tokio::fs.

use super::diff;
use std::path::Path;

/// File content with metadata.
#[derive(Debug, Clone)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub total_lines: usize,
    pub offset: usize,
    pub lines_returned: usize,
}

/// File metadata.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub path: String,
    pub size_bytes: u64,
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub modified: Option<u64>,
    pub readonly: bool,
}

/// Result of an edit operation.
#[derive(Debug, Clone)]
pub struct EditResult {
    pub replacements_made: usize,
}

/// Edit errors.
#[derive(Debug)]
pub enum EditError {
    Io(std::io::Error),
    /// The old text was not found in the file.
    NotFound,
    /// The old text appears multiple times and replace_all is false.
    AmbiguousMatch { count: usize },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::NotFound => write!(f, "old text not found in file"),
            Self::AmbiguousMatch { count } => {
                write!(f, "old text found {count} times (use replace_all=true to replace all)")
            }
        }
    }
}

impl std::error::Error for EditError {}

impl From<std::io::Error> for EditError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Read a file with optional line offset and limit.
///
/// Returns content formatted with 1-based line numbers (`cat -n` style).
/// `offset` is 0-based. `limit` of 0 means read all lines.
pub async fn read_file(
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<FileContent, std::io::Error> {
    let raw = tokio::fs::read_to_string(path).await?;
    let all_lines: Vec<&str> = raw.lines().collect();
    let total_lines = all_lines.len();

    let end = if limit == 0 {
        total_lines
    } else {
        (offset + limit).min(total_lines)
    };

    let selected = &all_lines[offset.min(total_lines)..end];
    let mut content = String::new();
    for (i, line) in selected.iter().enumerate() {
        let line_num = offset + i + 1;
        content.push_str(&format!("{:>6}\t{}\n", line_num, line));
    }

    Ok(FileContent {
        path: path.display().to_string(),
        content,
        total_lines,
        offset,
        lines_returned: selected.len(),
    })
}

/// Write content to a file, creating parent directories automatically.
pub async fn write_file(path: &Path, content: &str) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, content).await
}

/// String replacement in a file.
///
/// If `replace_all` is false and the old text appears more than once,
/// returns `EditError::AmbiguousMatch`. If old text is not found,
/// returns `EditError::NotFound`.
pub async fn edit_file(
    path: &Path,
    old_text: &str,
    new_text: &str,
    replace_all: bool,
) -> Result<EditResult, EditError> {
    let content = tokio::fs::read_to_string(path).await?;

    let count = content.matches(old_text).count();
    if count == 0 {
        return Err(EditError::NotFound);
    }
    if count > 1 && !replace_all {
        return Err(EditError::AmbiguousMatch { count });
    }

    let new_content = if replace_all {
        content.replace(old_text, new_text)
    } else {
        content.replacen(old_text, new_text, 1)
    };

    tokio::fs::write(path, &new_content).await?;

    Ok(EditResult {
        replacements_made: if replace_all { count } else { 1 },
    })
}

/// Produce a unified diff between the file's current content and proposed new content.
pub async fn diff_file(
    path: &Path,
    new_content: &str,
    context_lines: usize,
) -> Result<String, std::io::Error> {
    let old_content = tokio::fs::read_to_string(path).await?;
    Ok(diff::unified_diff(&old_content, new_content, context_lines))
}

/// Apply a unified diff patch to a file.
pub async fn patch_file(path: &Path, patch: &str) -> Result<(), PatchFileError> {
    let original = tokio::fs::read_to_string(path)
        .await
        .map_err(PatchFileError::Io)?;
    let patched =
        diff::apply_patch(&original, patch).map_err(|e| PatchFileError::Patch(e.message))?;
    tokio::fs::write(path, &patched)
        .await
        .map_err(PatchFileError::Io)?;
    Ok(())
}

/// Errors from patch_file.
#[derive(Debug)]
pub enum PatchFileError {
    Io(std::io::Error),
    Patch(String),
}

impl std::fmt::Display for PatchFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Patch(msg) => write!(f, "patch failed: {msg}"),
        }
    }
}

impl std::error::Error for PatchFileError {}

/// Check if a file exists.
pub async fn file_exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

/// Get file size in bytes.
pub async fn file_size(path: &Path) -> Result<u64, std::io::Error> {
    let meta = tokio::fs::metadata(path).await?;
    Ok(meta.len())
}

/// Get detailed file metadata.
pub async fn file_metadata(path: &Path) -> Result<FileMetadata, std::io::Error> {
    let meta = tokio::fs::metadata(path).await?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    Ok(FileMetadata {
        path: path.display().to_string(),
        size_bytes: meta.len(),
        is_file: meta.is_file(),
        is_dir: meta.is_dir(),
        is_symlink: meta.file_type().is_symlink(),
        modified,
        readonly: meta.permissions().readonly(),
    })
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_write() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");

        write_file(&path, "line1\nline2\nline3\n").await.unwrap();
        let fc = read_file(&path, 0, 0).await.unwrap();
        assert_eq!(fc.total_lines, 3);
        assert_eq!(fc.lines_returned, 3);
        assert!(fc.content.contains("line2"));
    }

    #[tokio::test]
    async fn test_read_with_offset() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        write_file(&path, "a\nb\nc\nd\ne\n").await.unwrap();

        let fc = read_file(&path, 2, 2).await.unwrap();
        assert_eq!(fc.lines_returned, 2);
        assert!(fc.content.contains("c"));
        assert!(fc.content.contains("d"));
        assert!(!fc.content.contains("a"));
    }

    #[tokio::test]
    async fn test_edit_single() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        write_file(&path, "hello world").await.unwrap();

        let result = edit_file(&path, "world", "earth", false).await.unwrap();
        assert_eq!(result.replacements_made, 1);

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "hello earth");
    }

    #[tokio::test]
    async fn test_edit_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        write_file(&path, "hello").await.unwrap();

        let result = edit_file(&path, "xyz", "abc", false).await;
        assert!(matches!(result, Err(EditError::NotFound)));
    }

    #[tokio::test]
    async fn test_edit_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        write_file(&path, "aaa bbb aaa").await.unwrap();

        let result = edit_file(&path, "aaa", "ccc", false).await;
        assert!(matches!(result, Err(EditError::AmbiguousMatch { count: 2 })));

        // replace_all works
        let result = edit_file(&path, "aaa", "ccc", true).await.unwrap();
        assert_eq!(result.replacements_made, 2);
    }

    #[tokio::test]
    async fn test_diff_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        write_file(&path, "hello\nworld\n").await.unwrap();

        let d = diff_file(&path, "hello\nearth\n", 3).await.unwrap();
        assert!(d.contains("-world"));
        assert!(d.contains("+earth"));
    }

    #[tokio::test]
    async fn test_file_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        write_file(&path, "content").await.unwrap();

        let meta = file_metadata(&path).await.unwrap();
        assert!(meta.is_file);
        assert!(!meta.is_dir);
        assert_eq!(meta.size_bytes, 7);
    }

    #[tokio::test]
    async fn test_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!file_exists(&tmp.path().join("nope")).await);

        let path = tmp.path().join("yes.txt");
        write_file(&path, "").await.unwrap();
        assert!(file_exists(&path).await);
    }
}
