//! Declarative command-output filter engine.
//!
//! Each filter is a pipeline of stages (strip_ansi → replace → match_output
//! → strip/keep_lines → truncate_lines_at → head/tail_lines → max_lines →
//! on_empty). Filters are keyed by a regex over the invoking command (the
//! first word(s) of a Bash command — e.g. `^git\s+log`).
//!
//! Credits: adapted from rtk (Rust Token Killer) — `rtk/src/core/toml_filter.rs`.
//! Trust-gating, disk lookup, and per-filter inline tests from rtk have been
//! removed — this build only consumes compile-time-embedded rules.
//! MIT © Patrick Szymkowiak. See LICENSE.

use once_cell::sync::Lazy;
use regex::{Regex, RegexSet};
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::ansi;

// ─── Embedded rules ────────────────────────────────────────────────────────

/// Every `src/rules/*.toml` file, parsed independently. Order here is
/// authoritative: earlier entries (`git`, `cargo`, …) win over the `generic`
/// catch-all at lookup time.
const BUILTIN_RULE_FILES: &[(&str, &str)] = &[
    ("git", include_str!("rules/git.toml")),
    ("cargo", include_str!("rules/cargo.toml")),
    ("npm", include_str!("rules/npm.toml")),
    ("pnpm", include_str!("rules/pnpm.toml")),
    ("pytest", include_str!("rules/pytest.toml")),
    ("docker", include_str!("rules/docker.toml")),
    ("generic", include_str!("rules/generic.toml")),
];

// ─── TOML schema ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchOutputRule {
    pattern: String,
    message: String,
    #[serde(default)]
    unless: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceRule {
    pattern: String,
    replacement: String,
}

#[derive(Deserialize)]
struct RuleFile {
    schema_version: u32,
    #[serde(default)]
    filters: BTreeMap<String, FilterDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilterDef {
    #[allow(dead_code)]
    description: Option<String>,
    match_command: String,
    #[serde(default)]
    strip_ansi: bool,
    #[serde(default)]
    replace: Vec<ReplaceRule>,
    #[serde(default)]
    match_output: Vec<MatchOutputRule>,
    #[serde(default)]
    strip_lines_matching: Vec<String>,
    #[serde(default)]
    keep_lines_matching: Vec<String>,
    truncate_lines_at: Option<usize>,
    head_lines: Option<usize>,
    tail_lines: Option<usize>,
    max_lines: Option<usize>,
    on_empty: Option<String>,
}

// ─── Compiled form ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct CompiledMatchOutput {
    pattern: Regex,
    message: String,
    unless: Option<Regex>,
}

#[derive(Debug)]
struct CompiledReplace {
    pattern: Regex,
    replacement: String,
}

#[derive(Debug)]
enum LineFilter {
    None,
    Strip(RegexSet),
    Keep(RegexSet),
}

#[derive(Debug)]
pub struct CompiledFilter {
    #[allow(dead_code)]
    pub name: String,
    match_regex: Regex,
    strip_ansi: bool,
    replace: Vec<CompiledReplace>,
    match_output: Vec<CompiledMatchOutput>,
    line_filter: LineFilter,
    truncate_lines_at: Option<usize>,
    head_lines: Option<usize>,
    tail_lines: Option<usize>,
    max_lines: Option<usize>,
    on_empty: Option<String>,
}

// ─── Loader ─────────────────────────────────────────────────────────────────

fn compile(name: String, def: FilterDef) -> Result<CompiledFilter, String> {
    if !def.strip_lines_matching.is_empty() && !def.keep_lines_matching.is_empty() {
        return Err("strip_lines_matching and keep_lines_matching are mutually exclusive".into());
    }
    let match_regex =
        Regex::new(&def.match_command).map_err(|e| format!("invalid match_command regex: {e}"))?;

    let replace = def
        .replace
        .into_iter()
        .map(|r| {
            let pat = r.pattern.clone();
            Regex::new(&r.pattern)
                .map(|pattern| CompiledReplace {
                    pattern,
                    replacement: r.replacement,
                })
                .map_err(|e| format!("invalid replace '{pat}': {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let match_output = def
        .match_output
        .into_iter()
        .map(|r| -> Result<CompiledMatchOutput, String> {
            let pat = r.pattern.clone();
            let pattern =
                Regex::new(&r.pattern).map_err(|e| format!("invalid match_output '{pat}': {e}"))?;
            let unless = r
                .unless
                .as_deref()
                .map(|u| {
                    Regex::new(u).map_err(|e| format!("invalid match_output unless '{u}': {e}"))
                })
                .transpose()?;
            Ok(CompiledMatchOutput {
                pattern,
                message: r.message,
                unless,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let line_filter = if !def.strip_lines_matching.is_empty() {
        let set = RegexSet::new(&def.strip_lines_matching)
            .map_err(|e| format!("invalid strip_lines_matching: {e}"))?;
        LineFilter::Strip(set)
    } else if !def.keep_lines_matching.is_empty() {
        let set = RegexSet::new(&def.keep_lines_matching)
            .map_err(|e| format!("invalid keep_lines_matching: {e}"))?;
        LineFilter::Keep(set)
    } else {
        LineFilter::None
    };

    Ok(CompiledFilter {
        name,
        match_regex,
        strip_ansi: def.strip_ansi,
        replace,
        match_output,
        line_filter,
        truncate_lines_at: def.truncate_lines_at,
        head_lines: def.head_lines,
        tail_lines: def.tail_lines,
        max_lines: def.max_lines,
        on_empty: def.on_empty,
    })
}

fn parse_and_compile(content: &str, source: &str) -> Result<Vec<CompiledFilter>, String> {
    let file: RuleFile =
        toml::from_str(content).map_err(|e| format!("TOML parse error in {source}: {e}"))?;
    if file.schema_version != 1 {
        return Err(format!(
            "unsupported schema_version {} in {source} (expected 1)",
            file.schema_version
        ));
    }
    let mut out = Vec::new();
    for (name, def) in file.filters {
        match compile(name.clone(), def) {
            Ok(f) => out.push(f),
            Err(e) => tracing::warn!("compression: filter '{name}' in {source}: {e}"),
        }
    }
    Ok(out)
}

static REGISTRY: Lazy<Vec<CompiledFilter>> = Lazy::new(|| {
    let mut out = Vec::new();
    for (source, content) in BUILTIN_RULE_FILES {
        match parse_and_compile(content, source) {
            Ok(f) => out.extend(f),
            Err(e) => tracing::warn!("compression: builtin rules '{source}' failed: {e}"),
        }
    }
    out
});

/// Look up the first filter matching `command`. O(N) over a small list.
pub fn find_matching(command: &str) -> Option<&'static CompiledFilter> {
    REGISTRY.iter().find(|f| f.match_regex.is_match(command))
}

// ─── Pipeline ───────────────────────────────────────────────────────────────

pub fn apply(filter: &CompiledFilter, stdout: &str) -> String {
    let mut lines: Vec<String> = stdout.lines().map(String::from).collect();

    // 1. strip_ansi
    if filter.strip_ansi {
        lines = lines.into_iter().map(|l| ansi::strip_ansi(&l)).collect();
    }

    // 2. replace (line-by-line, chainable)
    if !filter.replace.is_empty() {
        lines = lines
            .into_iter()
            .map(|mut line| {
                for rule in &filter.replace {
                    line = rule
                        .pattern
                        .replace_all(&line, rule.replacement.as_str())
                        .into_owned();
                }
                line
            })
            .collect();
    }

    // 3. match_output (short-circuit)
    if !filter.match_output.is_empty() {
        let blob = lines.join("\n");
        for rule in &filter.match_output {
            if rule.pattern.is_match(&blob) {
                if let Some(ref u) = rule.unless {
                    if u.is_match(&blob) {
                        continue;
                    }
                }
                return rule.message.clone();
            }
        }
    }

    // 4. strip / keep
    match &filter.line_filter {
        LineFilter::Strip(set) => lines.retain(|l| !set.is_match(l)),
        LineFilter::Keep(set) => lines.retain(|l| set.is_match(l)),
        LineFilter::None => {}
    }

    // 5. truncate_lines_at
    if let Some(n) = filter.truncate_lines_at {
        lines = lines.into_iter().map(|l| ansi::truncate(&l, n)).collect();
    }

    // 6. head / tail
    let total = lines.len();
    match (filter.head_lines, filter.tail_lines) {
        (Some(h), Some(t)) if total > h + t => {
            let mut r = lines[..h].to_vec();
            r.push(format!("... ({} lines omitted)", total - h - t));
            r.extend_from_slice(&lines[total - t..]);
            lines = r;
        }
        (Some(h), None) if total > h => {
            lines.truncate(h);
            lines.push(format!("... ({} lines omitted)", total - h));
        }
        (None, Some(t)) if total > t => {
            let omitted = total - t;
            lines = lines[omitted..].to_vec();
            lines.insert(0, format!("... ({omitted} lines omitted)"));
        }
        _ => {}
    }

    // 7. max_lines
    if let Some(max) = filter.max_lines {
        if lines.len() > max {
            let truncated = lines.len() - max;
            lines.truncate(max);
            lines.push(format!("... ({truncated} lines truncated)"));
        }
    }

    // 8. on_empty
    let result = lines.join("\n");
    if result.trim().is_empty() {
        if let Some(ref msg) = filter.on_empty {
            return msg.clone();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(toml: &str) -> Vec<CompiledFilter> {
        parse_and_compile(toml, "test").expect("valid test toml")
    }

    #[test]
    fn builtin_rules_compile() {
        let mut total = 0;
        for (source, content) in BUILTIN_RULE_FILES {
            let out = parse_and_compile(content, source)
                .unwrap_or_else(|e| panic!("{source} failed: {e}"));
            assert!(!out.is_empty(), "{source} had zero filters");
            total += out.len();
        }
        assert!(total >= 7);
    }

    #[test]
    fn short_circuit_match_output() {
        let f = mk(r#"
schema_version = 1
[filters.f]
match_command = "^x"
match_output = [ { pattern = "Already on", message = "ok" } ]
"#);
        assert_eq!(apply(&f[0], "Already on 'main'"), "ok");
    }

    #[test]
    fn strip_lines_and_ansi() {
        let f = mk(r#"
schema_version = 1
[filters.f]
match_command = "^x"
strip_ansi = true
strip_lines_matching = ["^noise"]
"#);
        let out = apply(&f[0], "\x1b[31mkeep\x1b[0m\nnoise line\nalso keep");
        assert_eq!(out, "keep\nalso keep");
    }

    #[test]
    fn head_tail_collapses_middle() {
        let f = mk(r#"
schema_version = 1
[filters.f]
match_command = "^x"
head_lines = 2
tail_lines = 2
"#);
        let src = "a\nb\nc\nd\ne\nf";
        let out = apply(&f[0], src);
        assert!(out.starts_with("a\nb\n"));
        assert!(out.contains("2 lines omitted"));
        assert!(out.ends_with("e\nf"));
    }

    #[test]
    fn builtin_git_log_matches() {
        let hit = find_matching("git log --oneline -20");
        assert!(hit.is_some(), "git rule should match `git log`");
    }

    #[test]
    fn builtin_cargo_build_matches() {
        let hit = find_matching("cargo build --release");
        assert!(hit.is_some(), "cargo rule should match `cargo build`");
    }
}
