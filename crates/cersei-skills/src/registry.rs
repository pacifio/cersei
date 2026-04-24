//! `SkillRegistry` — scan filesystem for SKILL.md files and expose them.
//!
//! Two layers, merged with **project-local wins on name collision**:
//!
//! 1. **User-global:** `~/.cersei/skills/*/SKILL.md`
//! 2. **Project-local:** `.cersei/skills/*/SKILL.md` (relative to cwd)
//!
//! No writes happen here — mutating skills is `cersei-tools::skills`
//! territory. This crate only reads.

use crate::parser::{parse_skill, Skill, SkillMeta, SkillParseError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a skill was loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrySource {
    Global,
    Project,
}

#[derive(Debug)]
pub struct SkillRegistry {
    /// name → (skill, source). Only the winning copy is stored (project beats
    /// global on name collision).
    skills: BTreeMap<String, (Skill, RegistrySource)>,
    /// Parse failures, tuple of (path, error string). Kept around for
    /// reporting; loader never bails on a single bad skill.
    errors: Vec<(PathBuf, String)>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self {
            skills: BTreeMap::new(),
            errors: Vec::new(),
        }
    }
}

impl SkillRegistry {
    /// Empty registry (useful in tests).
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from the conventional locations: `~/.cersei/skills/` first (as
    /// global), then `.cersei/skills/` in the cwd (as project, overriding).
    pub fn from_defaults() -> anyhow::Result<Self> {
        let mut reg = Self::default();
        if let Some(home) = dirs::home_dir() {
            let global_dir = home.join(".cersei").join("skills");
            if global_dir.exists() {
                reg.load_dir(&global_dir, RegistrySource::Global);
            }
        }
        let project_dir = std::env::current_dir()?
            .join(".cersei")
            .join("skills");
        if project_dir.exists() {
            reg.load_dir(&project_dir, RegistrySource::Project);
        }
        Ok(reg)
    }

    /// Load every SKILL.md under `root`. Parse failures are collected into
    /// `self.errors` but do not abort the load.
    pub fn load_dir(&mut self, root: &Path, source: RegistrySource) {
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .max_depth(4)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file()
                && entry
                    .path()
                    .file_name()
                    .is_some_and(|n| n == "SKILL.md")
            {
                self.load_one(entry.path(), source);
            }
        }
    }

    /// Load exactly one SKILL.md file.
    pub fn load_one(&mut self, path: &Path, source: RegistrySource) {
        match std::fs::read_to_string(path) {
            Err(e) => self.errors.push((path.to_path_buf(), e.to_string())),
            Ok(content) => match parse_skill(&content, path) {
                Err(e) => self.errors.push((path.to_path_buf(), e.to_string())),
                Ok(skill) => self.insert(skill, source),
            },
        }
    }

    fn insert(&mut self, skill: Skill, source: RegistrySource) {
        let name = skill.frontmatter.name.clone();
        match self.skills.get(&name) {
            // Project wins over global on collision; otherwise last-write-wins.
            Some((_, RegistrySource::Project)) if source == RegistrySource::Global => {}
            _ => {
                self.skills.insert(name, (skill, source));
            }
        }
    }

    /// Progressive-disclosure list — token-cheap, safe to stream into a
    /// system prompt every turn.
    pub fn list(&self) -> Vec<SkillMeta> {
        let mut out: Vec<_> = self.skills.values().map(|(s, _)| s.meta()).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Full skill lookup by name. Returns None if missing.
    pub fn view(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name).map(|(s, _)| s)
    }

    /// `(skill, source)` lookup — useful when the caller needs to know
    /// whether a skill came from global or project scope.
    pub fn view_with_source(&self, name: &str) -> Option<(&Skill, RegistrySource)> {
        self.skills.get(name).map(|(s, src)| (s, *src))
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Parse failures encountered during load. Callers can surface these
    /// to users so they know a skill didn't load.
    pub fn errors(&self) -> &[(PathBuf, String)] {
        &self.errors
    }

    /// Find skills whose `metadata.cersei.tags` overlap with any of `tags`.
    /// Token-cheap prompt-time filter for "inject relevant skill hints".
    pub fn by_tag(&self, tags: &[&str]) -> Vec<SkillMeta> {
        self.list()
            .into_iter()
            .filter(|m| m.tags.iter().any(|t| tags.contains(&t.as_str())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_skill(dir: &Path, name: &str, body: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), body).unwrap();
    }

    const FIXTURE_A: &str = "---
name: alpha
description: First skill
version: \"1.0.0\"
license: MIT
metadata:
  cersei:
    tags: [test]
---
body-a";

    const FIXTURE_B: &str = "---
name: beta
description: Second skill
version: \"0.1.0\"
license: MIT
---
body-b";

    const FIXTURE_A_PROJECT: &str = "---
name: alpha
description: Project override of alpha
version: \"2.0.0\"
license: MIT
---
project-body-a";

    #[test]
    fn loads_multiple_skills() {
        let td = TempDir::new().unwrap();
        write_skill(td.path(), "alpha", FIXTURE_A);
        write_skill(td.path(), "beta", FIXTURE_B);
        let mut reg = SkillRegistry::new();
        reg.load_dir(td.path(), RegistrySource::Global);
        assert_eq!(reg.len(), 2);
        let names: Vec<_> = reg.list().into_iter().map(|m| m.name).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn project_overrides_global() {
        let global = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_skill(global.path(), "alpha", FIXTURE_A);
        write_skill(project.path(), "alpha", FIXTURE_A_PROJECT);

        let mut reg = SkillRegistry::new();
        reg.load_dir(global.path(), RegistrySource::Global);
        reg.load_dir(project.path(), RegistrySource::Project);

        let (skill, src) = reg.view_with_source("alpha").unwrap();
        assert_eq!(src, RegistrySource::Project);
        assert_eq!(skill.frontmatter.version, "2.0.0");
        assert!(skill.body.contains("project-body-a"));
    }

    #[test]
    fn global_does_not_override_project() {
        let global = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_skill(project.path(), "alpha", FIXTURE_A_PROJECT);
        write_skill(global.path(), "alpha", FIXTURE_A);

        let mut reg = SkillRegistry::new();
        // Load project first, then global — global must not win.
        reg.load_dir(project.path(), RegistrySource::Project);
        reg.load_dir(global.path(), RegistrySource::Global);

        let (_, src) = reg.view_with_source("alpha").unwrap();
        assert_eq!(src, RegistrySource::Project);
    }

    #[test]
    fn by_tag_filters() {
        let td = TempDir::new().unwrap();
        write_skill(td.path(), "alpha", FIXTURE_A);
        write_skill(td.path(), "beta", FIXTURE_B);
        let mut reg = SkillRegistry::new();
        reg.load_dir(td.path(), RegistrySource::Global);
        let hits = reg.by_tag(&["test"]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "alpha");
    }

    #[test]
    fn bad_skill_does_not_abort_load() {
        let td = TempDir::new().unwrap();
        write_skill(td.path(), "alpha", FIXTURE_A);
        write_skill(td.path(), "broken", "no frontmatter");
        let mut reg = SkillRegistry::new();
        reg.load_dir(td.path(), RegistrySource::Global);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.errors().len(), 1);
        assert!(reg.errors()[0].1.contains("frontmatter"));
    }
}
