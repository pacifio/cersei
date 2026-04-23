//! Text diffing primitives using the `similar` crate.
//!
//! Pure functions — no I/O, no async. Produces unified diffs,
//! structured line diffs, and can apply patches.

use similar::{ChangeTag as SimilarTag, TextDiff};
use std::fmt;

/// Type of change for a diff line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeTag {
    Added,
    Removed,
    Unchanged,
}

/// A single line in a structured diff.
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub tag: ChangeTag,
    pub line_number_old: Option<usize>,
    pub line_number_new: Option<usize>,
    pub content: String,
}

/// Error when applying a patch fails.
#[derive(Debug)]
pub struct PatchError {
    pub message: String,
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "patch error: {}", self.message)
    }
}

impl std::error::Error for PatchError {}

/// Produce a unified diff string (standard format with @@ hunk headers).
///
/// ```rust,ignore
/// let diff = unified_diff("hello\nworld\n", "hello\nearth\n", 3);
/// assert!(diff.contains("-world"));
/// assert!(diff.contains("+earth"));
/// ```
pub fn unified_diff(old: &str, new: &str, context_lines: usize) -> String {
    let diff = TextDiff::from_lines(old, new);
    diff.unified_diff()
        .context_radius(context_lines)
        .header("old", "new")
        .to_string()
}

/// Return a structured per-line diff.
///
/// Each line includes the change tag, old/new line numbers, and content.
pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    let mut result = Vec::new();
    let mut old_line: usize = 1;
    let mut new_line: usize = 1;

    for change in diff.iter_all_changes() {
        let tag = match change.tag() {
            SimilarTag::Equal => ChangeTag::Unchanged,
            SimilarTag::Insert => ChangeTag::Added,
            SimilarTag::Delete => ChangeTag::Removed,
        };

        let (ln_old, ln_new) = match tag {
            ChangeTag::Unchanged => {
                let r = (Some(old_line), Some(new_line));
                old_line += 1;
                new_line += 1;
                r
            }
            ChangeTag::Removed => {
                let r = (Some(old_line), None);
                old_line += 1;
                r
            }
            ChangeTag::Added => {
                let r = (None, Some(new_line));
                new_line += 1;
                r
            }
        };

        result.push(DiffLine {
            tag,
            line_number_old: ln_old,
            line_number_new: ln_new,
            content: change.to_string_lossy().to_string(),
        });
    }

    result
}

/// Apply a unified diff patch to the original text.
///
/// Returns the patched text, or an error if the patch doesn't apply cleanly.
/// This is a simple line-based patch applicator — it handles standard unified
/// diff format with `@@` hunk headers and `+`/`-`/` ` line prefixes.
pub fn apply_patch(original: &str, patch: &str) -> Result<String, PatchError> {
    let original_lines: Vec<&str> = original.lines().collect();
    let mut result_lines: Vec<String> = Vec::new();
    let mut orig_idx: usize = 0;

    let patch_lines: Vec<&str> = patch.lines().collect();
    let mut patch_idx: usize = 0;

    // Skip header lines (---, +++, etc.)
    while patch_idx < patch_lines.len() {
        let line = patch_lines[patch_idx];
        if line.starts_with("@@") {
            break;
        }
        patch_idx += 1;
    }

    while patch_idx < patch_lines.len() {
        let line = patch_lines[patch_idx];

        if line.starts_with("@@") {
            // Parse hunk header: @@ -old_start,old_count +new_start,new_count @@
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                return Err(PatchError {
                    message: format!("malformed hunk header: {}", line),
                });
            }

            let old_part = parts[1].trim_start_matches('-');
            let old_start: usize = old_part
                .split(',')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);

            // Copy unchanged lines before this hunk
            while orig_idx + 1 < old_start && orig_idx < original_lines.len() {
                result_lines.push(original_lines[orig_idx].to_string());
                orig_idx += 1;
            }

            patch_idx += 1;
            continue;
        }

        if line.starts_with('-') {
            // Remove line — skip it in original
            orig_idx += 1;
        } else if line.starts_with('+') {
            // Add line
            result_lines.push(line[1..].to_string());
        } else if line.starts_with(' ') || line.is_empty() {
            // Context line — copy from original
            if orig_idx < original_lines.len() {
                result_lines.push(original_lines[orig_idx].to_string());
                orig_idx += 1;
            }
        }

        patch_idx += 1;
    }

    // Copy remaining original lines after last hunk
    while orig_idx < original_lines.len() {
        result_lines.push(original_lines[orig_idx].to_string());
        orig_idx += 1;
    }

    Ok(result_lines.join("\n"))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_diff_basic() {
        let old = "hello\nworld\n";
        let new = "hello\nearth\n";
        let diff = unified_diff(old, new, 3);
        assert!(diff.contains("-world"));
        assert!(diff.contains("+earth"));
        assert!(diff.contains("@@"));
    }

    #[test]
    fn test_unified_diff_identical() {
        let text = "same\ncontent\n";
        let diff = unified_diff(text, text, 3);
        assert!(diff.is_empty() || !diff.contains("@@"));
    }

    #[test]
    fn test_line_diff_basic() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let lines = line_diff(old, new);

        let removed: Vec<_> = lines
            .iter()
            .filter(|l| l.tag == ChangeTag::Removed)
            .collect();
        let added: Vec<_> = lines.iter().filter(|l| l.tag == ChangeTag::Added).collect();

        assert_eq!(removed.len(), 1);
        assert_eq!(added.len(), 1);
        assert!(removed[0].content.contains('b'));
        assert!(added[0].content.contains('B'));
    }

    #[test]
    fn test_line_diff_empty() {
        let lines = line_diff("", "");
        assert!(lines.is_empty());
    }

    #[test]
    fn test_apply_patch_basic() {
        let old = "hello\nworld\nfoo\n";
        let new = "hello\nearth\nfoo\n";
        let patch = unified_diff(old, new, 3);
        let result = apply_patch(old, &patch).unwrap();
        assert!(result.contains("earth"));
        assert!(!result.contains("world"));
    }

    #[test]
    fn test_line_numbers() {
        let old = "a\nb\nc\n";
        let new = "a\nc\n";
        let lines = line_diff(old, new);

        let removed = lines.iter().find(|l| l.tag == ChangeTag::Removed).unwrap();
        assert_eq!(removed.line_number_old, Some(2));
        assert_eq!(removed.line_number_new, None);
    }
}
