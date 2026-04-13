//! Memory directory: persistent file-based memory system.
//!
//! Scans .md files with YAML frontmatter, sorts by recency, caps at 200 files.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ─── Types ───────────────────────────────────────────────────────────────────

/// Memory types for categorized memory entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }
}

/// Metadata for a memory file (without loading full content).
#[derive(Debug, Clone)]
pub struct MemoryFileMeta {
    pub filename: String,
    pub path: PathBuf,
    pub name: Option<String>,
    pub description: Option<String>,
    pub memory_type: Option<MemoryType>,
    pub modified_secs: u64,
}

/// A memory file with its full content.
#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub meta: MemoryFileMeta,
    pub content: String,
}

/// Result of loading MEMORY.md with truncation info.
pub struct MemoryIndex {
    pub content: String,
    pub truncated: bool,
    pub total_lines: usize,
}

// ─── Constants ───────────────────────────────────────────────────────────────

const MAX_MEMORY_FILES: usize = 200;
const MAX_INDEX_LINES: usize = 200;
const MAX_INDEX_BYTES: usize = 25_000;

// ─── Path resolution ─────────────────────────────────────────────────────────

/// Sanitize a path component for use as a directory name.
/// Keeps alphanumeric, `-`, `_`, `.`. Replaces everything else with `_`.
pub fn sanitize_path_component(path: &str) -> String {
    path.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Resolve the memory directory for a project.
/// Default: `~/.claude/projects/<sanitized-root>/memory/`
pub fn auto_memory_path(project_root: &Path) -> PathBuf {
    // Check override env var first
    if let Ok(override_path) = std::env::var("CERSEI_MEMORY_PATH_OVERRIDE") {
        return PathBuf::from(override_path);
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let sanitized = sanitize_path_component(&project_root.display().to_string());
    home.join(".claude").join("projects").join(sanitized).join("memory")
}

/// Create the memory directory if it doesn't exist.
pub fn ensure_memory_dir_exists(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::debug!("Failed to create memory dir {}: {}", dir.display(), e);
    }
}

// ─── Scanning ────────────────────────────────────────────────────────────────

/// Parse YAML frontmatter from the first ~30 lines of a file.
/// Returns (name, description, memory_type).
fn parse_frontmatter_quick(content: &str) -> (Option<String>, Option<String>, Option<MemoryType>) {
    let mut name = None;
    let mut description = None;
    let mut memory_type = None;

    if !content.starts_with("---") {
        return (name, description, memory_type);
    }

    let mut in_frontmatter = false;
    for (i, line) in content.lines().enumerate() {
        if i > 30 { break; }
        if i == 0 && line.trim() == "---" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter && line.trim() == "---" {
            break;
        }
        if !in_frontmatter { continue; }

        if let Some(colon) = line.find(':') {
            let key = line[..colon].trim().to_lowercase();
            let value = line[colon + 1..].trim().to_string();
            match key.as_str() {
                "name" => name = Some(value),
                "description" => description = Some(value),
                "type" => memory_type = MemoryType::from_str(&value),
                _ => {}
            }
        }
    }

    (name, description, memory_type)
}

/// Scan a memory directory for .md files.
/// Returns metadata sorted newest-first, capped at 200 files.
/// Excludes MEMORY.md (the index file).
pub fn scan_memory_dir(dir: &Path) -> Vec<MemoryFileMeta> {
    let mut results: Vec<MemoryFileMeta> = Vec::new();

    let _walker = match std::fs::read_dir(dir) {
        Ok(w) => w,
        Err(_) => return results,
    };

    // Recursive scan
    scan_dir_recursive(dir, dir, &mut results);

    // Sort newest-first
    results.sort_by(|a, b| b.modified_secs.cmp(&a.modified_secs));

    // Cap
    results.truncate(MAX_MEMORY_FILES);
    results
}

fn scan_dir_recursive(base: &Path, dir: &Path, results: &mut Vec<MemoryFileMeta>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            scan_dir_recursive(base, &path, results);
            continue;
        }

        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        // Skip MEMORY.md (index file)
        if path.file_name().and_then(|n| n.to_str()) == Some("MEMORY.md") {
            continue;
        }

        let filename = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .display()
            .to_string();

        let modified_secs = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Quick frontmatter parse
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let (name, description, memory_type) = parse_frontmatter_quick(&content);

        results.push(MemoryFileMeta {
            filename,
            path,
            name,
            description,
            memory_type,
            modified_secs,
        });
    }
}

// ─── Index loading ───────────────────────────────────────────────────────────

/// Load the MEMORY.md index file with truncation.
/// Returns None if the file doesn't exist or is empty.
pub fn load_memory_index(memory_dir: &Path) -> Option<MemoryIndex> {
    let index_path = memory_dir.join("MEMORY.md");
    let content = std::fs::read_to_string(&index_path).ok()?;

    if content.trim().is_empty() {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let truncated = total_lines > MAX_INDEX_LINES || content.len() > MAX_INDEX_BYTES;

    let output = if truncated {
        let mut result: String = lines[..MAX_INDEX_LINES.min(total_lines)]
            .join("\n");
        if result.len() > MAX_INDEX_BYTES {
            result.truncate(MAX_INDEX_BYTES);
        }
        result.push_str(&format!(
            "\n\n<!-- MEMORY.md truncated: {} total lines, showing {} -->",
            total_lines, MAX_INDEX_LINES.min(total_lines)
        ));
        result
    } else {
        content
    };

    Some(MemoryIndex {
        content: output,
        truncated,
        total_lines,
    })
}

/// Build the complete memory prompt content for the system prompt.
pub fn build_memory_prompt_content(memory_dir: &Path) -> String {
    let index = load_memory_index(memory_dir);
    match index {
        Some(idx) => idx.content,
        None => String::new(),
    }
}

// ─── Staleness ───────────────────────────────────────────────────────────────

/// How many days ago a memory was modified.
pub fn memory_age_days(modified_secs: u64) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now > modified_secs {
        (now - modified_secs) / 86400
    } else {
        0
    }
}

/// Human-readable age string.
pub fn memory_age_text(modified_secs: u64) -> String {
    let days = memory_age_days(modified_secs);
    match days {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        d => format!("{} days ago", d),
    }
}

/// Freshness warning for stale memories (>1 day old).
pub fn memory_freshness_text(modified_secs: u64) -> Option<String> {
    let days = memory_age_days(modified_secs);
    if days > 1 {
        Some(format!(
            "This memory was last updated {} — verify it's still current before acting on it.",
            memory_age_text(modified_secs)
        ))
    } else {
        None
    }
}

// ─── Load full file ──────────────────────────────────────────────────────────

/// Load a memory file with its full content, stripping frontmatter.
pub fn load_memory_file(path: &Path) -> Option<MemoryFile> {
    let content = std::fs::read_to_string(path).ok()?;
    let (name, description, memory_type) = parse_frontmatter_quick(&content);

    let modified_secs = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Strip frontmatter from content
    let body = crate::strip_frontmatter(&content);

    Some(MemoryFile {
        meta: MemoryFileMeta {
            filename: path.file_name()?.to_str()?.to_string(),
            path: path.to_path_buf(),
            name,
            description,
            memory_type,
            modified_secs,
        },
        content: body,
    })
}


// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn create_memory_file(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn test_scan_memory_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mem_dir = tmp.path();

        create_memory_file(mem_dir, "user_role.md", "---\nname: User Role\ndescription: Developer preferences\ntype: user\n---\n\nI prefer Rust.");
        create_memory_file(mem_dir, "project_arch.md", "---\nname: Architecture\ntype: project\n---\n\nMicroservices.");
        create_memory_file(mem_dir, "feedback_style.md", "---\ntype: feedback\n---\n\nBe concise.");
        create_memory_file(mem_dir, "MEMORY.md", "- [User Role](user_role.md)\n- [Architecture](project_arch.md)");
        create_memory_file(mem_dir, "no_frontmatter.md", "Just plain content.");

        let metas = scan_memory_dir(mem_dir);
        assert_eq!(metas.len(), 4); // excludes MEMORY.md
        assert!(metas.iter().all(|m| m.filename != "MEMORY.md"));

        // Check frontmatter parsing
        let user = metas.iter().find(|m| m.filename == "user_role.md").unwrap();
        assert_eq!(user.name.as_deref(), Some("User Role"));
        assert_eq!(user.description.as_deref(), Some("Developer preferences"));
        assert_eq!(user.memory_type, Some(MemoryType::User));

        // No frontmatter still scanned
        let plain = metas.iter().find(|m| m.filename == "no_frontmatter.md").unwrap();
        assert!(plain.name.is_none());
    }

    #[test]
    fn test_load_memory_index() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "- [Test](test.md) — hook\n".repeat(10)).unwrap();

        let index = load_memory_index(tmp.path());
        assert!(index.is_some());
        let index = index.unwrap();
        assert!(!index.truncated);
        assert_eq!(index.total_lines, 10);
    }

    #[test]
    fn test_load_memory_index_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let content = "- line\n".repeat(300);
        std::fs::write(tmp.path().join("MEMORY.md"), content).unwrap();

        let index = load_memory_index(tmp.path());
        assert!(index.is_some());
        let index = index.unwrap();
        assert!(index.truncated);
        assert!(index.content.contains("truncated"));
    }

    #[test]
    fn test_load_memory_index_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_memory_index(tmp.path()).is_none());
    }

    #[test]
    fn test_sanitize_path_component() {
        assert_eq!(sanitize_path_component("/Users/foo/project"), "_Users_foo_project");
        assert_eq!(sanitize_path_component("simple-name"), "simple-name");
        assert_eq!(sanitize_path_component("a/b:c"), "a_b_c");
    }

    #[test]
    fn test_memory_age() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(memory_age_days(now), 0);
        assert_eq!(memory_age_text(now), "today");
        assert_eq!(memory_age_text(now - 86400), "yesterday");
        assert_eq!(memory_age_text(now - 86400 * 5), "5 days ago");
    }

    #[test]
    fn test_freshness_warning() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(memory_freshness_text(now).is_none()); // today = fresh
        assert!(memory_freshness_text(now - 86400 * 3).is_some()); // 3 days = stale
    }

    #[test]
    fn test_auto_memory_path() {
        let path = auto_memory_path(Path::new("/Users/test/myproject"));
        assert!(path.to_str().unwrap().contains("memory"));
        assert!(path.to_str().unwrap().contains(".claude"));
    }

    #[test]
    fn test_load_memory_file() {
        let tmp = tempfile::tempdir().unwrap();
        create_memory_file(tmp.path(), "test.md", "---\nname: Test\ntype: user\n---\n\nContent here.");

        let file = load_memory_file(&tmp.path().join("test.md"));
        assert!(file.is_some());
        let file = file.unwrap();
        assert_eq!(file.meta.name.as_deref(), Some("Test"));
        assert!(file.content.contains("Content here"));
        assert!(!file.content.contains("name: Test")); // frontmatter stripped
    }

    #[test]
    fn test_build_memory_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "- [Role](role.md) — my role\n- [Project](proj.md) — the project").unwrap();

        let content = build_memory_prompt_content(tmp.path());
        assert!(content.contains("Role"));
        assert!(content.contains("Project"));
    }
}
