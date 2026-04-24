//! SKILL.md parser — YAML frontmatter + markdown body.
//!
//! Frontmatter schema (matches agentskills.io spec):
//!
//! ```yaml
//! ---
//! name: run-tests              # required, ≤64 chars, kebab-case
//! description: >-              # required, ≤1024 chars
//!   Run the project's test suite and summarise failures.
//! version: "1.0.0"             # semver, required
//! license: MIT                 # SPDX identifier, required
//! metadata:                    # optional, free-form; `.cersei.tags` is special
//!   cersei:
//!     tags: [testing, ci]
//! platforms: [macos, linux]    # optional allow-list
//! prerequisites:               # optional
//!   env_vars: [CARGO_TERM_COLOR]
//!   commands: [cargo]
//! ---
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const MAX_NAME_LEN: usize = 64;
pub const MAX_DESCRIPTION_LEN: usize = 1024;
pub const MAX_SKILL_BYTES: usize = 100 * 1024;

/// Lightweight skill metadata — what `SkillRegistry::list()` returns. Small
/// enough to stream into a system prompt on every turn without blowing up
/// token budgets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
    pub version: String,
}

/// Full skill: frontmatter + markdown body + source file path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
    pub source_path: PathBuf,
}

impl Skill {
    pub fn meta(&self) -> SkillMeta {
        SkillMeta {
            name: self.frontmatter.name.clone(),
            description: self.frontmatter.description.clone(),
            tags: self.frontmatter.cersei_tags(),
            platforms: self.frontmatter.platforms.clone(),
            version: self.frontmatter.version.clone(),
        }
    }

    /// Directory containing this skill (parent of SKILL.md).
    pub fn dir(&self) -> Option<&Path> {
        self.source_path.parent()
    }
}

/// Full YAML frontmatter, as parsed. Free-form `metadata` object is preserved
/// as `serde_json::Value` so skill authors can attach arbitrary data without
/// breaking the loader.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    pub version: String,
    pub license: String,

    #[serde(default)]
    pub platforms: Vec<String>,

    #[serde(default)]
    pub prerequisites: Option<SkillPrerequisites>,

    /// Free-form metadata. `metadata.cersei.tags` is the canonical path for
    /// Cersei-specific tags; we expose it via `cersei_tags()`.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl SkillFrontmatter {
    /// Extract `metadata.cersei.tags` if present.
    pub fn cersei_tags(&self) -> Vec<String> {
        self.metadata
            .get("cersei")
            .and_then(|c| c.get("tags"))
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillPrerequisites {
    #[serde(default)]
    pub env_vars: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Error)]
pub enum SkillParseError {
    #[error("skill file exceeds {} KB limit ({size} bytes)", MAX_SKILL_BYTES / 1024)]
    TooLarge { size: usize },

    #[error("no YAML frontmatter: file must start with `---` on its own line")]
    MissingFrontmatter,

    #[error("unterminated YAML frontmatter (no closing `---`)")]
    UnterminatedFrontmatter,

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("invalid skill: {0}")]
    Invalid(String),
}

/// Parse raw SKILL.md bytes into a `Skill`. `source` records the file path
/// we loaded from for better error messages upstream.
pub fn parse_skill(content: &str, source: &Path) -> Result<Skill, SkillParseError> {
    if content.len() > MAX_SKILL_BYTES {
        return Err(SkillParseError::TooLarge {
            size: content.len(),
        });
    }

    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Err(SkillParseError::MissingFrontmatter);
    }
    // Advance past the opening `---\n`.
    let after_open = &trimmed[3..];
    let after_open = after_open.trim_start_matches('\n');

    // Find the closing `---` line.
    let close_idx = after_open
        .find("\n---")
        .ok_or(SkillParseError::UnterminatedFrontmatter)?;
    let yaml_slice = &after_open[..close_idx];
    let after_close = &after_open[close_idx + 4..];
    let body = after_close.trim_start_matches(|c: char| c == '\n' || c == '\r');

    let fm: SkillFrontmatter = serde_yaml::from_str(yaml_slice)?;
    validate(&fm)?;

    Ok(Skill {
        frontmatter: fm,
        body: body.to_string(),
        source_path: source.to_path_buf(),
    })
}

fn validate(fm: &SkillFrontmatter) -> Result<(), SkillParseError> {
    if fm.name.trim().is_empty() {
        return Err(SkillParseError::Invalid("name is required".into()));
    }
    if fm.name.chars().count() > MAX_NAME_LEN {
        return Err(SkillParseError::Invalid(format!(
            "name exceeds {MAX_NAME_LEN} chars"
        )));
    }
    if !fm.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(SkillParseError::Invalid(
            "name must be ASCII alphanumerics / `-` / `_` only".into(),
        ));
    }
    if fm.description.trim().is_empty() {
        return Err(SkillParseError::Invalid("description is required".into()));
    }
    if fm.description.chars().count() > MAX_DESCRIPTION_LEN {
        return Err(SkillParseError::Invalid(format!(
            "description exceeds {MAX_DESCRIPTION_LEN} chars"
        )));
    }
    if fm.version.trim().is_empty() {
        return Err(SkillParseError::Invalid("version is required".into()));
    }
    if fm.license.trim().is_empty() {
        return Err(SkillParseError::Invalid("license is required".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const FIXTURE: &str = "---
name: run-tests
description: Run the cargo test suite and summarise failures
version: \"1.0.0\"
license: MIT
metadata:
  cersei:
    tags: [testing, rust]
platforms:
  - macos
  - linux
prerequisites:
  commands: [cargo]
  env_vars: []
---

# run-tests

Steps:
1. `cargo test --workspace`
2. Parse failures.
";

    #[test]
    fn parses_full_frontmatter() {
        let s = parse_skill(FIXTURE, &PathBuf::from("SKILL.md")).unwrap();
        assert_eq!(s.frontmatter.name, "run-tests");
        assert_eq!(s.frontmatter.version, "1.0.0");
        assert_eq!(s.frontmatter.license, "MIT");
        assert_eq!(s.frontmatter.platforms, vec!["macos", "linux"]);
        assert_eq!(s.frontmatter.cersei_tags(), vec!["testing", "rust"]);
        assert!(s.body.contains("# run-tests"));
        assert!(s.body.contains("cargo test --workspace"));
    }

    #[test]
    fn meta_is_minimal() {
        let s = parse_skill(FIXTURE, &PathBuf::from("SKILL.md")).unwrap();
        let m = s.meta();
        assert_eq!(m.name, "run-tests");
        assert_eq!(m.tags, vec!["testing", "rust"]);
    }

    #[test]
    fn rejects_missing_frontmatter() {
        let err = parse_skill("no frontmatter here", &PathBuf::from("x.md")).unwrap_err();
        assert!(matches!(err, SkillParseError::MissingFrontmatter));
    }

    #[test]
    fn rejects_unterminated_frontmatter() {
        let err = parse_skill("---\nname: x\n", &PathBuf::from("x.md")).unwrap_err();
        assert!(matches!(err, SkillParseError::UnterminatedFrontmatter));
    }

    #[test]
    fn rejects_too_large() {
        let big = format!(
            "---\nname: x\ndescription: y\nversion: 1.0.0\nlicense: MIT\n---\n{}",
            "z".repeat(MAX_SKILL_BYTES + 1)
        );
        let err = parse_skill(&big, &PathBuf::from("x.md")).unwrap_err();
        assert!(matches!(err, SkillParseError::TooLarge { .. }));
    }

    #[test]
    fn rejects_empty_required_fields() {
        let bad = "---\nname: ''\ndescription: d\nversion: 1.0.0\nlicense: MIT\n---\nbody";
        let err = parse_skill(bad, &PathBuf::from("x.md")).unwrap_err();
        assert!(matches!(err, SkillParseError::Invalid(_)));
    }

    #[test]
    fn rejects_non_kebab_name() {
        let bad = "---\nname: 'has spaces'\ndescription: d\nversion: 1.0.0\nlicense: MIT\n---\nbody";
        let err = parse_skill(bad, &PathBuf::from("x.md")).unwrap_err();
        assert!(matches!(err, SkillParseError::Invalid(_)));
    }
}
