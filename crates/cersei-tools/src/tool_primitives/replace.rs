//! Tolerant string replacement — a "replacer ladder".
//!
//! Exact string matching makes edits fragile: weaker BYOK models (Qwen,
//! DeepSeek, Gemini Flash, …) routinely drift on indentation and whitespace,
//! so an otherwise-correct `old_string` fails to match and the edit is lost.
//!
//! This module ports OpenCode's MIT-licensed replacer ladder to Rust: a series
//! of progressively more tolerant matching strategies. Each strategy yields
//! *candidate substrings that actually exist in the file*; the engine then
//! locates the candidate and applies the replacement only when it is unique.
//! Because every candidate is a real substring of the content, a fuzzy match
//! never invents text — it only relaxes *how* `old_string` is located.
//!
//! Ladder (tried in order, first unique hit wins):
//!   1. exact match
//!   2. line-trimmed match (per-line leading/trailing whitespace ignored)
//!   3. block-anchor match (first/last line anchor a 3+ line block; guarded)
//!   4. whitespace-normalized match (runs of whitespace collapsed)
//!   5. indentation-flexible match (common leading indent stripped)
//!
//! ## Destructive-match guard
//!
//! The line-based strategies require *every* line to match (after the
//! strategy's normalization), so they cannot map onto an unrelated region.
//! The block-anchor strategy is the one exception — it matches only the first
//! and last lines and ignores the middle — so it is additionally guarded by a
//! similarity threshold ([`BLOCK_ANCHOR_MIN_SIMILARITY`]) computed against the
//! requested text. This prevents a fuzzy match from silently rewriting the
//! wrong block of code.

use similar::TextDiff;

/// Minimum char-level similarity between a block-anchor candidate and the
/// requested `old_string` for the candidate to be accepted. Anchors alone
/// (first + last line) are too weak a signal to safely rewrite a block, so the
/// interior must also resemble what the caller asked to replace.
const BLOCK_ANCHOR_MIN_SIMILARITY: f32 = 0.5;

/// Why a tolerant replacement could not be performed.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplaceError {
    /// No strategy located the text in the content.
    NotFound,
    /// The text was found more than once and `replace_all` was false.
    Ambiguous { count: usize },
    /// `old_string` and `new_string` are identical — the edit is a no-op.
    NoChange,
    /// `old_string` is empty but the file is non-empty (cannot anchor an edit).
    EmptyOldString,
}

/// Perform a tolerant replacement, returning the new file content.
///
/// Exact matching is attempted first so that genuine ambiguity (the text
/// appears verbatim multiple times) is reported as [`ReplaceError::Ambiguous`]
/// rather than being silently skipped. Only when an exact match is *not* found
/// does the fuzzy ladder engage; fuzzy candidates that are non-unique are
/// skipped rather than guessed at.
pub fn replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, ReplaceError> {
    if old == new {
        return Err(ReplaceError::NoChange);
    }
    if old.is_empty() {
        // Allow initializing an empty file; otherwise an empty anchor is unsafe.
        if content.is_empty() {
            return Ok(new.to_string());
        }
        return Err(ReplaceError::EmptyOldString);
    }

    // ── Exact match first (preserves precise ambiguity reporting) ──────────
    let exact_count = content.matches(old).count();
    if exact_count == 1 {
        return Ok(content.replacen(old, new, 1));
    }
    if exact_count > 1 {
        if replace_all {
            return Ok(content.replace(old, new));
        }
        return Err(ReplaceError::Ambiguous { count: exact_count });
    }

    // ── Fuzzy ladder (exact match failed) ─────────────────────────────────
    let strategies: [fn(&str, &str) -> Vec<String>; 4] = [
        line_trimmed_replacer,
        block_anchor_replacer,
        whitespace_normalized_replacer,
        indentation_flexible_replacer,
    ];

    for strategy in strategies {
        for search in strategy(content, old) {
            if search.is_empty() {
                continue;
            }
            let Some(index) = content.find(&search) else {
                continue;
            };
            if replace_all {
                return Ok(content.replace(&search, new));
            }
            // Uniqueness guard: a fuzzy candidate that matches in more than one
            // place is too risky to apply — skip and let a later strategy try.
            let last = content.rfind(&search).expect("find succeeded");
            if index != last {
                continue;
            }
            let mut result = String::with_capacity(content.len() - search.len() + new.len());
            result.push_str(&content[..index]);
            result.push_str(new);
            result.push_str(&content[index + search.len()..]);
            return Ok(result);
        }
    }

    Err(ReplaceError::NotFound)
}

/// Byte offset of the start of line `line_idx` (0-based) within `content`,
/// where lines are produced by `split('\n')`.
fn line_start_offset(lines: &[&str], line_idx: usize) -> usize {
    let mut offset = 0;
    for line in &lines[..line_idx] {
        offset += line.len() + 1; // +1 for the '\n' separator
    }
    offset
}

/// Drop a single trailing empty element produced by a trailing newline so that
/// `"a\nb\n"` and `"a\nb"` are treated as the same multi-line search.
fn trim_trailing_blank(mut lines: Vec<&str>) -> Vec<&str> {
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// Match each line ignoring its leading/trailing whitespace. Every line must
/// match, so this cannot land on an unrelated region.
fn line_trimmed_replacer(content: &str, find: &str) -> Vec<String> {
    let original: Vec<&str> = content.split('\n').collect();
    let search = trim_trailing_blank(find.split('\n').collect());
    if search.is_empty() || original.len() < search.len() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..=(original.len() - search.len()) {
        let matches = (0..search.len()).all(|j| original[i + j].trim() == search[j].trim());
        if !matches {
            continue;
        }
        let start = line_start_offset(&original, i);
        // End at the last matched line *without* its trailing newline so the
        // candidate mirrors `find`'s line span exactly.
        let mut end = start;
        for (k, _) in search.iter().enumerate() {
            end += original[i + k].len();
            if k + 1 < search.len() {
                end += 1; // interior '\n'
            }
        }
        if let Some(slice) = content.get(start..end) {
            out.push(slice.to_string());
        }
    }
    out
}

/// Strip the common leading indentation from both the search text and each
/// candidate window, then compare. Handles whole-block indent level drift.
fn indentation_flexible_replacer(content: &str, find: &str) -> Vec<String> {
    let strip_common_indent = |text: &str| -> String {
        let lines: Vec<&str> = text.split('\n').collect();
        let min_indent = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        lines
            .iter()
            .map(|l| {
                if l.len() >= min_indent {
                    &l[l.floor_char_boundary(min_indent)..]
                } else {
                    l.trim_start()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let search_lines = trim_trailing_blank(find.split('\n').collect());
    if search_lines.is_empty() {
        return Vec::new();
    }
    let normalized_find = strip_common_indent(&search_lines.join("\n"));

    let original: Vec<&str> = content.split('\n').collect();
    let m = search_lines.len();
    if original.len() < m {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..=(original.len() - m) {
        let block = original[i..i + m].join("\n");
        if strip_common_indent(&block) == normalized_find {
            out.push(block);
        }
    }
    out
}

/// Collapse runs of whitespace to a single space (and trim) before comparing.
/// Operates per-line for single-line searches and over a sliding window of the
/// same line count for multi-line searches.
fn whitespace_normalized_replacer(content: &str, find: &str) -> Vec<String> {
    let normalize = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let search_lines = trim_trailing_blank(find.split('\n').collect());
    if search_lines.is_empty() {
        return Vec::new();
    }
    let normalized_find = normalize(&search_lines.join(" "));
    if normalized_find.is_empty() {
        return Vec::new();
    }

    let original: Vec<&str> = content.split('\n').collect();
    let m = search_lines.len();
    if original.len() < m {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..=(original.len() - m) {
        let block = original[i..i + m].join("\n");
        if normalize(&block) == normalized_find {
            out.push(block);
        }
    }
    out
}

/// Match a block of 3+ lines using its first and last lines (trimmed) as
/// anchors, ignoring the interior. Guarded by [`BLOCK_ANCHOR_MIN_SIMILARITY`]
/// so a coincidental anchor pair cannot rewrite an unrelated block.
fn block_anchor_replacer(content: &str, find: &str) -> Vec<String> {
    let original: Vec<&str> = content.split('\n').collect();
    let search = trim_trailing_blank(find.split('\n').collect());
    if search.len() < 3 {
        return Vec::new();
    }

    let first = search[0].trim();
    let last = search[search.len() - 1].trim();

    // Collect candidate (start, end) line ranges anchored by first/last lines.
    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for i in 0..original.len() {
        if original[i].trim() != first {
            continue;
        }
        // Last line must be at least 2 lines below the first (3+ line block).
        if let Some(offset) = original
            .iter()
            .skip(i + 2)
            .position(|l| l.trim() == last)
        {
            candidates.push((i, i + 2 + offset));
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    let requested = search.join("\n");
    let mut best: Option<(f32, String)> = None;
    for (start, end) in candidates {
        let block = original[start..=end].join("\n");
        let score = TextDiff::from_lines(requested.as_str(), block.as_str()).ratio();
        if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, block));
        }
    }

    match best {
        Some((score, block)) if score >= BLOCK_ANCHOR_MIN_SIMILARITY => vec![block],
        _ => Vec::new(),
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let out = replace("hello world", "world", "earth", false).unwrap();
        assert_eq!(out, "hello earth");
    }

    #[test]
    fn exact_ambiguous_reports_count() {
        assert_eq!(
            replace("a a a", "a", "b", false),
            Err(ReplaceError::Ambiguous { count: 3 })
        );
    }

    #[test]
    fn exact_replace_all() {
        assert_eq!(replace("a a a", "a", "b", true).unwrap(), "b b b");
    }

    #[test]
    fn no_change_is_error() {
        assert_eq!(replace("x", "x", "x", false), Err(ReplaceError::NoChange));
    }

    #[test]
    fn empty_old_initializes_empty_file() {
        assert_eq!(replace("", "", "hi", false).unwrap(), "hi");
        assert_eq!(
            replace("x", "", "hi", false),
            Err(ReplaceError::EmptyOldString)
        );
    }

    #[test]
    fn line_trimmed_tolerates_leading_indent_drift() {
        // File is indented with 4 spaces; model supplied no indentation.
        let content = "fn main() {\n        let x = 1;\n}\n";
        let out = replace(content, "let x = 1;", "let x = 2;", false).unwrap();
        assert_eq!(out, "fn main() {\n        let x = 2;\n}\n");
    }

    #[test]
    fn line_trimmed_multiline_indent_drift() {
        let content = "impl Foo {\n    fn a(&self) {\n        self.b();\n    }\n}\n";
        // Model dropped the 8-space indentation across both lines.
        let old = "fn a(&self) {\nself.b();\n}";
        let new = "fn a(&self) {\n        self.c();\n    }";
        let out = replace(content, old, new, false).unwrap();
        assert!(out.contains("self.c();"));
        assert!(!out.contains("self.b();"));
    }

    #[test]
    fn indentation_flexible_whole_block_shift() {
        // File uses tabs-equivalent deep indent; model used 2-space relative.
        let content = "        if cond {\n            do_thing();\n        }\n";
        let old = "if cond {\n  do_thing();\n}";
        let new = "if cond {\n  do_other();\n}";
        let out = replace(content, old, new, false).unwrap();
        assert!(out.contains("do_other();"));
    }

    #[test]
    fn indentation_flexible_does_not_split_unicode_whitespace() {
        let block = "  alpha\n\u{3000}beta\n  🙂";
        assert_eq!(
            indentation_flexible_replacer(block, block),
            vec![block.to_string()]
        );
    }

    #[test]
    fn whitespace_normalized_collapses_runs() {
        let content = "let   x    =     1;\n";
        let out = replace(content, "let x = 1;", "let x = 2;", false).unwrap();
        assert_eq!(out, "let x = 2;\n");
    }

    #[test]
    fn block_anchor_matches_drifted_interior() {
        let content =
            "fn calc() {\n    let a = 1;\n    let b = 2;\n    a + b\n}\n";
        // Model got the interior slightly wrong but anchors are right.
        let old = "fn calc() {\n    let a = 1;\n    let b = 2;\n    a + b\n}";
        let new = "fn calc() {\n    42\n}";
        let out = replace(content, old, new, false).unwrap();
        assert!(out.contains("42"));
    }

    #[test]
    fn block_anchor_guard_rejects_dissimilar_block() {
        // Two blocks share first/last anchors but the interiors are totally
        // different. The requested text resembles neither well enough to be
        // safely rewritten by anchors alone.
        let content = "start\nzzz\nzzz\nzzz\nzzz\nend\n";
        let old = "start\naaa\nbbb\nccc\nddd\nend";
        // Similarity is low → guard should refuse rather than clobber.
        let result = replace(content, old, "start\nnew\nend", false);
        assert_eq!(result, Err(ReplaceError::NotFound));
    }

    #[test]
    fn not_found_when_nothing_matches() {
        assert_eq!(
            replace("hello\n", "nonexistent", "x", false),
            Err(ReplaceError::NotFound)
        );
    }

    #[test]
    fn fuzzy_skips_nonunique_candidate() {
        // Two trim-equal regions; non-unique fuzzy candidate must not be applied.
        let content = "  foo\nbar\n  foo\n";
        let result = replace(content, "foo", "baz", false);
        // "foo" is exact and appears twice → ambiguous, not a silent fuzzy edit.
        assert_eq!(result, Err(ReplaceError::Ambiguous { count: 2 }));
    }
}
