//! Language-aware code filtering.
//!
//! Credits: adapted from rtk (Rust Token Killer) — `rtk/src/core/filter.rs`.
//! MIT © Patrick Szymkowiak. See LICENSE.

use crate::level::CompressionLevel;
use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    C,
    Cpp,
    Java,
    Ruby,
    Shell,
    /// JSON / YAML / TOML / XML / CSV — never code-stripped.
    Data,
    Unknown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Language::Rust,
            "py" | "pyw" => Language::Python,
            "js" | "mjs" | "cjs" => Language::JavaScript,
            "ts" | "tsx" => Language::TypeScript,
            "go" => Language::Go,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" => Language::Cpp,
            "java" => Language::Java,
            "rb" => Language::Ruby,
            "sh" | "bash" | "zsh" => Language::Shell,
            "json" | "jsonc" | "json5" | "yaml" | "yml" | "toml" | "xml" | "csv" | "tsv"
            | "graphql" | "gql" | "sql" | "md" | "markdown" | "txt" | "env" | "lock" => {
                Language::Data
            }
            _ => Language::Unknown,
        }
    }

    pub fn from_path(path: &str) -> Self {
        let ext = path.rsplit('.').next().unwrap_or("");
        if ext == path || ext.is_empty() {
            Language::Unknown
        } else {
            Self::from_extension(ext)
        }
    }

    fn comment_patterns(&self) -> CommentPatterns {
        match self {
            Language::Rust => CommentPatterns {
                line: Some("//"),
                block_start: Some("/*"),
                block_end: Some("*/"),
                doc_line: Some("///"),
                doc_block_start: Some("/**"),
            },
            Language::Python => CommentPatterns {
                line: Some("#"),
                block_start: Some("\"\"\""),
                block_end: Some("\"\"\""),
                doc_line: None,
                doc_block_start: Some("\"\"\""),
            },
            Language::JavaScript
            | Language::TypeScript
            | Language::Go
            | Language::C
            | Language::Cpp
            | Language::Java => CommentPatterns {
                line: Some("//"),
                block_start: Some("/*"),
                block_end: Some("*/"),
                doc_line: None,
                doc_block_start: Some("/**"),
            },
            Language::Ruby => CommentPatterns {
                line: Some("#"),
                block_start: Some("=begin"),
                block_end: Some("=end"),
                doc_line: None,
                doc_block_start: None,
            },
            Language::Shell => CommentPatterns {
                line: Some("#"),
                block_start: None,
                block_end: None,
                doc_line: None,
                doc_block_start: None,
            },
            Language::Data | Language::Unknown => CommentPatterns::default(),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct CommentPatterns {
    line: Option<&'static str>,
    block_start: Option<&'static str>,
    block_end: Option<&'static str>,
    doc_line: Option<&'static str>,
    doc_block_start: Option<&'static str>,
}

static MULTIPLE_BLANK_LINES: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());
static IMPORT_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(use |import |from |require\(|#include)").unwrap());
static FUNC_SIGNATURE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^(pub\s+)?(async\s+)?(fn|def|function|func|class|struct|enum|trait|interface|type)\s+\w+",
    )
    .unwrap()
});

/// Apply code-aware filtering. For `Data` or `Unknown` languages, returns input
/// unchanged to avoid corrupting JSON/YAML/TOML (rtk issue #464).
pub fn filter(content: &str, lang: Language, level: CompressionLevel) -> String {
    match level {
        CompressionLevel::Off => content.to_string(),
        CompressionLevel::Minimal => minimal(content, lang),
        CompressionLevel::Aggressive => aggressive(content, lang),
    }
}

fn minimal(content: &str, lang: Language) -> String {
    if matches!(lang, Language::Data | Language::Unknown) {
        return content.to_string();
    }
    let patterns = lang.comment_patterns();
    let mut out = String::with_capacity(content.len());
    let mut in_block_comment = false;
    let mut in_docstring = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Handle block comments
        if let (Some(start), Some(end)) = (patterns.block_start, patterns.block_end) {
            if !in_docstring
                && trimmed.contains(start)
                && !trimmed.starts_with(patterns.doc_block_start.unwrap_or("\0"))
            {
                in_block_comment = true;
            }
            if in_block_comment {
                if trimmed.contains(end) {
                    in_block_comment = false;
                }
                continue;
            }
        }

        // Python docstrings: keep in minimal mode
        if lang == Language::Python && trimmed.starts_with("\"\"\"") {
            in_docstring = !in_docstring;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_docstring {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Single-line comments (keep doc comments if language has them)
        if let Some(line_comment) = patterns.line {
            if trimmed.starts_with(line_comment) {
                if let Some(doc) = patterns.doc_line {
                    if trimmed.starts_with(doc) {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                continue;
            }
        }

        if trimmed.is_empty() {
            out.push('\n');
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    MULTIPLE_BLANK_LINES
        .replace_all(&out, "\n\n")
        .trim()
        .to_string()
}

fn aggressive(content: &str, lang: Language) -> String {
    if matches!(lang, Language::Data | Language::Unknown) {
        return minimal(content, lang);
    }
    let minimal_out = minimal(content, lang);
    let mut out = String::with_capacity(minimal_out.len() / 2);
    let mut brace_depth: i32 = 0;
    let mut in_impl_body = false;

    for line in minimal_out.lines() {
        let trimmed = line.trim();

        if IMPORT_PATTERN.is_match(trimmed) {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if FUNC_SIGNATURE.is_match(trimmed) {
            out.push_str(line);
            out.push('\n');
            in_impl_body = true;
            brace_depth = 0;
            continue;
        }

        let open = trimmed.matches('{').count() as i32;
        let close = trimmed.matches('}').count() as i32;

        if in_impl_body {
            brace_depth += open;
            brace_depth -= close;

            if brace_depth <= 1 && (trimmed == "{" || trimmed == "}" || trimmed.ends_with('{')) {
                out.push_str(line);
                out.push('\n');
            }

            if brace_depth <= 0 {
                in_impl_body = false;
                if !trimmed.is_empty() && trimmed != "}" {
                    out.push_str("    // ... implementation\n");
                }
            }
            continue;
        }

        if trimmed.starts_with("const ")
            || trimmed.starts_with("static ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("pub const ")
            || trimmed.starts_with("pub static ")
        {
            out.push_str(line);
            out.push('\n');
        }
    }

    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_detection() {
        assert_eq!(Language::from_extension("rs"), Language::Rust);
        assert_eq!(Language::from_extension("py"), Language::Python);
        assert_eq!(Language::from_extension("json"), Language::Data);
        assert_eq!(Language::from_extension("lock"), Language::Data);
        assert_eq!(Language::from_extension("xyz"), Language::Unknown);
        assert_eq!(Language::from_path("/a/b/c.rs"), Language::Rust);
        assert_eq!(Language::from_path("Dockerfile"), Language::Unknown);
    }

    #[test]
    fn minimal_strips_rust_line_comments_keeps_doc() {
        let src = "\
// normal comment
/// doc comment
fn main() {
    println!(\"hi\");
}
";
        let out = minimal(src, Language::Rust);
        assert!(!out.contains("// normal comment"));
        assert!(out.contains("/// doc comment"));
        assert!(out.contains("fn main()"));
    }

    #[test]
    fn minimal_preserves_json() {
        let json =
            r#"{"pkgs": ["packages/*"], "scripts": {"build": "bun run --workspaces build"}}"#;
        assert_eq!(minimal(json, Language::Data), json);
    }

    #[test]
    fn aggressive_preserves_signatures_and_imports() {
        let src = "\
use std::io;
fn do_thing() {
    let x = 1;
    println!(\"{}\", x);
}
";
        let out = aggressive(src, Language::Rust);
        assert!(out.contains("use std::io"));
        assert!(out.contains("fn do_thing"));
        assert!(out.contains("... implementation") || !out.contains("println"));
    }
}
