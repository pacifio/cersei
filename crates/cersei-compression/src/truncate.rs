//! Line-structure-aware truncation — preserves function signatures and
//! imports, omits the body.
//!
//! Credits: adapted from rtk (Rust Token Killer) — `rtk/src/core/filter.rs`.
//! MIT © Patrick Szymkowiak. See LICENSE.

use once_cell::sync::Lazy;
use regex::Regex;

static IMPORT_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(use |import |from |require\(|#include)").unwrap());
static FUNC_SIGNATURE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^(pub\s+)?(async\s+)?(fn|def|function|func|class|struct|enum|trait|interface|type)\s+\w+",
    )
    .unwrap()
});

pub fn smart_truncate(content: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= max_lines {
        return content.to_string();
    }

    let mut out: Vec<String> = Vec::with_capacity(max_lines);
    let mut kept = 0usize;
    let mut skipped = false;

    for line in &lines {
        let trimmed = line.trim();
        let important = FUNC_SIGNATURE.is_match(trimmed)
            || IMPORT_PATTERN.is_match(trimmed)
            || trimmed.starts_with("pub ")
            || trimmed.starts_with("export ")
            || trimmed == "}"
            || trimmed == "{";

        if important || kept < max_lines / 2 {
            if skipped {
                out.push(format!(
                    "    // ... {} lines omitted",
                    lines.len().saturating_sub(kept)
                ));
                skipped = false;
            }
            out.push((*line).to_string());
            kept += 1;
        } else {
            skipped = true;
        }

        if kept >= max_lines.saturating_sub(1) {
            break;
        }
    }

    if skipped || kept < lines.len() {
        out.push(format!(
            "// ... {} more lines (total: {})",
            lines.len().saturating_sub(kept),
            lines.len()
        ));
    }

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_when_short() {
        let src = "a\nb\nc";
        assert_eq!(smart_truncate(src, 100), src);
    }

    #[test]
    fn truncates_and_counts() {
        let src: String = (0..200)
            .map(|i| format!("plain text line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = smart_truncate(&src, 20);
        let overflow = out
            .lines()
            .find(|l| l.contains("more lines"))
            .expect("overflow line");
        let n: usize = overflow
            .split_whitespace()
            .find_map(|w| w.parse().ok())
            .expect("count");
        let kept = out
            .lines()
            .filter(|l| !l.contains("more lines") && !l.contains("omitted"))
            .count();
        assert_eq!(kept + n, 200);
    }
}
