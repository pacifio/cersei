//! Skills system: discover, load, and execute skill prompt templates.
//!
//! Compatible with:
//! - **Claude Code**: `.claude/commands/*.md` with `$ARGUMENTS` expansion
//! - **OpenCode**: `.claude/skills/**/SKILL.md` with YAML frontmatter
//! - **Custom directories**: any path passed to the scanner

pub mod bundled;
pub mod discovery;

use serde::{Deserialize, Serialize};

/// Metadata about a discovered skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    /// Skill name (filename without .md, or frontmatter `name:` field).
    pub name: String,
    /// One-line description (from frontmatter or first content line).
    pub description: String,
    /// Absolute path to the .md file (None for bundled skills).
    pub path: Option<String>,
    /// Whether this is a built-in bundled skill.
    pub bundled: bool,
    /// Alternative names for this skill.
    pub aliases: Vec<String>,
    /// Tool restrictions: None = all tools, Some = only these tools.
    pub allowed_tools: Option<Vec<String>>,
    /// Usage hint for arguments.
    pub argument_hint: Option<String>,
    /// Skill format detected.
    pub format: SkillFormat,
}

/// Which format the skill file uses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SkillFormat {
    /// Claude Code format: .claude/commands/<name>.md
    ClaudeCode,
    /// OpenCode format: .claude/skills/<name>/SKILL.md with required frontmatter
    OpenCode,
    /// Bundled (compiled into binary)
    Bundled,
}

/// A loaded skill ready for expansion.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub meta: SkillMeta,
    /// Raw content (frontmatter stripped).
    pub content: String,
}

impl LoadedSkill {
    /// Expand the skill template with given arguments.
    pub fn expand(&self, args: Option<&str>) -> String {
        let mut result = self.content.clone();

        // Replace $ARGUMENTS_SUFFIX first (it contains $ARGUMENTS as substring)
        if let Some(args) = args {
            result = result.replace("$ARGUMENTS_SUFFIX", &format!(": {}", args));
            result = result.replace("$ARGUMENTS", args);
        } else {
            result = result.replace("$ARGUMENTS_SUFFIX", "");
            result = result.replace("$ARGUMENTS", "");
        }

        result
    }
}

/// Strip YAML frontmatter from content (Claude Code compatible).
/// Handles `---\n...\n---\n` format.
pub fn strip_frontmatter(content: &str) -> String {
    if content.starts_with("---") {
        let after_open = &content[3..];
        if let Some(close_pos) = after_open.find("\n---") {
            let rest = &after_open[close_pos + 4..];
            return rest.trim_start_matches('\n').to_string();
        }
    }
    content.to_string()
}

/// Parse YAML frontmatter into key-value pairs.
/// Returns (frontmatter_map, content_after_frontmatter).
pub fn parse_frontmatter(content: &str) -> (std::collections::HashMap<String, String>, String) {
    let mut map = std::collections::HashMap::new();

    if !content.starts_with("---") {
        return (map, content.to_string());
    }

    let after_open = &content[3..];
    if let Some(close_pos) = after_open.find("\n---") {
        let yaml_block = &after_open[..close_pos].trim();
        let body = after_open[close_pos + 4..].trim_start_matches('\n').to_string();

        // Simple YAML key: value parser (handles single-line values)
        for line in yaml_block.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(colon_pos) = line.find(':') {
                let key = line[..colon_pos].trim().to_string();
                let value = line[colon_pos + 1..].trim().to_string();
                map.insert(key, value);
            }
        }

        return (map, body);
    }

    (map, content.to_string())
}

/// Extract description from content: first non-empty line (max 80 chars).
/// Headings are stripped of their `#` prefix.
pub fn extract_description(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "---" {
            continue;
        }
        // Strip heading markers
        let trimmed = trimmed.trim_start_matches('#').trim();
        let desc = if trimmed.len() > 80 {
            format!("{}...", &trimmed[..77])
        } else {
            trimmed.to_string()
        };
        return desc;
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_frontmatter_with_yaml() {
        let content = "---\nname: test\ndescription: A test skill\n---\n\n# Body\n\nContent here.";
        let stripped = strip_frontmatter(content);
        assert!(stripped.starts_with("# Body"));
        assert!(!stripped.contains("name: test"));
    }

    #[test]
    fn test_strip_frontmatter_without_yaml() {
        let content = "# Just a heading\n\nSome content.";
        let stripped = strip_frontmatter(content);
        assert_eq!(stripped, content);
    }

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\nname: my-skill\ndescription: Does things\nallowed-tools: Read, Write\n---\n\nBody";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("name").unwrap(), "my-skill");
        assert_eq!(fm.get("description").unwrap(), "Does things");
        assert!(body.starts_with("Body"));
    }

    #[test]
    fn test_expand_with_arguments() {
        let skill = LoadedSkill {
            meta: SkillMeta {
                name: "test".into(),
                description: "test".into(),
                path: None,
                bundled: true,
                aliases: vec![],
                allowed_tools: None,
                argument_hint: None,
                format: SkillFormat::Bundled,
            },
            content: "Do $ARGUMENTS in the codebase$ARGUMENTS_SUFFIX".into(),
        };

        let expanded = skill.expand(Some("fix tests"));
        assert_eq!(expanded, "Do fix tests in the codebase: fix tests");

        let expanded_empty = skill.expand(None);
        assert_eq!(expanded_empty, "Do  in the codebase");
    }

    #[test]
    fn test_extract_description() {
        assert_eq!(
            extract_description("# Heading\n\nFirst real line here."),
            "Heading"
        );
        // extract_description works on raw content — frontmatter lines are skipped by the --- check
        assert_eq!(
            extract_description(&strip_frontmatter("---\nfoo\n---\nContent after FM")),
            "Content after FM"
        );
        assert_eq!(extract_description(""), "");
    }
}
