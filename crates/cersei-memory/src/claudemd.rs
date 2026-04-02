//! CLAUDE.md hierarchical loading.
//!
//! Loads instruction files from 4 scopes with priority merging:
//! 1. Managed: ~/.claude/rules/*.md
//! 2. User: ~/.claude/CLAUDE.md
//! 3. Project: {root}/CLAUDE.md
//! 4. Local: {root}/.claude/CLAUDE.md

use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ─── Types ───────────────────────────────────────────────────────────────────

/// Scope of a CLAUDE.md file (highest priority first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryScope {
    Managed = 0,
    User = 1,
    Project = 2,
    Local = 3,
}

/// A loaded CLAUDE.md / rules file with metadata.
#[derive(Debug, Clone)]
pub struct MemoryFileInfo {
    pub path: PathBuf,
    pub scope: MemoryScope,
    pub content: String,
    pub mtime: u64,
}

// ─── Constants ───────────────────────────────────────────────────────────────

const MAX_INCLUDE_DEPTH: usize = 10;
const MAX_INCLUDE_SIZE: usize = 40_000; // 40KB

// ─── Loading ─────────────────────────────────────────────────────────────────

/// Load all CLAUDE.md / rules files for a project, in priority order.
pub fn load_all_memory_files(project_root: &Path) -> Vec<MemoryFileInfo> {
    let mut files = Vec::new();
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

    // 1. Managed: ~/.claude/rules/*.md (sorted alphabetically)
    let rules_dir = home.join(".claude").join("rules");
    if rules_dir.exists() {
        let mut rule_files: Vec<PathBuf> = std::fs::read_dir(&rules_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
            .map(|e| e.path())
            .collect();
        rule_files.sort();
        for path in rule_files {
            if let Some(info) = load_memory_file(&path, MemoryScope::Managed) {
                files.push(info);
            }
        }
    }

    // 2. User: ~/.claude/CLAUDE.md
    let user_path = home.join(".claude").join("CLAUDE.md");
    if let Some(info) = load_memory_file(&user_path, MemoryScope::User) {
        files.push(info);
    }

    // 3. Project: {root}/CLAUDE.md
    let project_path = project_root.join("CLAUDE.md");
    if let Some(info) = load_memory_file(&project_path, MemoryScope::Project) {
        files.push(info);
    }

    // 4. Local: {root}/.claude/CLAUDE.md
    let local_path = project_root.join(".claude").join("CLAUDE.md");
    if let Some(info) = load_memory_file(&local_path, MemoryScope::Local) {
        files.push(info);
    }

    files
}

/// Load a single memory file with @include expansion.
fn load_memory_file(path: &Path, scope: MemoryScope) -> Option<MemoryFileInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }

    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Strip frontmatter
    let body = crate::strip_frontmatter(&content);

    // Expand @include directives
    let base_dir = path.parent().unwrap_or(Path::new("."));
    let mut visited = HashSet::new();
    visited.insert(path.to_path_buf());
    let expanded = expand_includes(&body, base_dir, &mut visited, 0);

    Some(MemoryFileInfo {
        path: path.to_path_buf(),
        scope,
        content: expanded,
        mtime,
    })
}

/// Expand `@include <path>` directives in content.
/// Supports:
/// - Relative paths (resolved from base_dir)
/// - ~ home directory expansion
/// - Max depth of 10 levels
/// - Max include size of 40KB
/// - Circular reference detection
fn expand_includes(
    content: &str,
    base_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> String {
    if depth >= MAX_INCLUDE_DEPTH {
        return content.to_string();
    }

    let mut result = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("@include ") {
            let include_path = trimmed.strip_prefix("@include ").unwrap().trim();

            // Expand ~ to home directory
            let expanded_path = if include_path.starts_with("~/") {
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                home.join(&include_path[2..])
            } else {
                base_dir.join(include_path)
            };

            // Circular reference check
            if visited.contains(&expanded_path) {
                result.push_str(&format!("<!-- circular @include: {} -->\n", include_path));
                continue;
            }

            // Size check
            if let Ok(meta) = std::fs::metadata(&expanded_path) {
                if meta.len() > MAX_INCLUDE_SIZE as u64 {
                    result.push_str(&format!(
                        "<!-- @include too large: {} ({} bytes, max {}) -->\n",
                        include_path,
                        meta.len(),
                        MAX_INCLUDE_SIZE
                    ));
                    continue;
                }
            }

            // Load and recurse
            if let Ok(included_content) = std::fs::read_to_string(&expanded_path) {
                visited.insert(expanded_path.clone());
                let included_dir = expanded_path.parent().unwrap_or(base_dir);
                let expanded = expand_includes(&included_content, included_dir, visited, depth + 1);
                result.push_str(&expanded);
                result.push('\n');
            } else {
                result.push_str(&format!("<!-- @include not found: {} -->\n", include_path));
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

/// Build a merged memory prompt from all loaded files.
pub fn build_memory_prompt(files: &[MemoryFileInfo]) -> String {
    files
        .iter()
        .filter(|f| !f.content.trim().is_empty())
        .map(|f| f.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_project_claude_md() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "# Project Rules\n\nUse Rust.").unwrap();

        let files = load_all_memory_files(tmp.path());
        let project = files.iter().find(|f| f.scope == MemoryScope::Project);
        assert!(project.is_some());
        assert!(project.unwrap().content.contains("Use Rust"));
    }

    #[test]
    fn test_load_local_claude_md() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("CLAUDE.md"), "Local overrides here.").unwrap();

        let files = load_all_memory_files(tmp.path());
        let local = files.iter().find(|f| f.scope == MemoryScope::Local);
        assert!(local.is_some());
        assert!(local.unwrap().content.contains("Local overrides"));
    }

    #[test]
    fn test_scope_ordering() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "Project").unwrap();
        let claude_dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("CLAUDE.md"), "Local").unwrap();

        let files = load_all_memory_files(tmp.path());
        // Project comes before Local in the list (lower scope number = higher priority)
        let project_idx = files.iter().position(|f| f.scope == MemoryScope::Project);
        let local_idx = files.iter().position(|f| f.scope == MemoryScope::Local);
        if let (Some(pi), Some(li)) = (project_idx, local_idx) {
            assert!(pi < li);
        }
    }

    #[test]
    fn test_include_expansion() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.md"), "Before\n@include extra.md\nAfter").unwrap();
        std::fs::write(tmp.path().join("extra.md"), "INCLUDED CONTENT").unwrap();

        let mut visited = HashSet::new();
        visited.insert(tmp.path().join("main.md"));
        let content = std::fs::read_to_string(tmp.path().join("main.md")).unwrap();
        let expanded = expand_includes(&content, tmp.path(), &mut visited, 0);

        assert!(expanded.contains("Before"));
        assert!(expanded.contains("INCLUDED CONTENT"));
        assert!(expanded.contains("After"));
    }

    #[test]
    fn test_circular_include() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "@include b.md").unwrap();
        std::fs::write(tmp.path().join("b.md"), "@include a.md").unwrap();

        let mut visited = HashSet::new();
        visited.insert(tmp.path().join("a.md"));
        let content = std::fs::read_to_string(tmp.path().join("a.md")).unwrap();
        let expanded = expand_includes(&content, tmp.path(), &mut visited, 0);

        assert!(expanded.contains("circular"));
    }

    #[test]
    fn test_build_memory_prompt() {
        let files = vec![
            MemoryFileInfo {
                path: PathBuf::from("a.md"),
                scope: MemoryScope::Managed,
                content: "Rule 1".into(),
                mtime: 0,
            },
            MemoryFileInfo {
                path: PathBuf::from("b.md"),
                scope: MemoryScope::Project,
                content: "Rule 2".into(),
                mtime: 0,
            },
        ];
        let prompt = build_memory_prompt(&files);
        assert!(prompt.contains("Rule 1"));
        assert!(prompt.contains("Rule 2"));
        assert!(prompt.contains("\n\n")); // separator
    }

    #[test]
    fn test_empty_file_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "").unwrap();

        let files = load_all_memory_files(tmp.path());
        assert!(files.iter().all(|f| f.scope != MemoryScope::Project));
    }

    #[test]
    fn test_frontmatter_stripped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "---\nscope: project\n---\n\nActual content.").unwrap();

        let files = load_all_memory_files(tmp.path());
        let project = files.iter().find(|f| f.scope == MemoryScope::Project).unwrap();
        assert!(project.content.contains("Actual content"));
        assert!(!project.content.contains("scope: project"));
    }
}
