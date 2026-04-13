//! Skill discovery: scan directories for .md skill files.
//!
//! Supports commands format (.claude/commands/*.md)
//! and skills format (.claude/skills/**/SKILL.md).

use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Default discovery directories relative to a project root.
const COMMANDS_DIRS: &[&str] = &[".claude/commands"];
const SKILLS_DIRS: &[&str] = &[".claude/skills", ".agents/skills"];

/// Scan all standard directories for skills.
///
/// Order: bundled > project-level > home-level > extra paths.
/// Deduplicates by name (first found wins).
pub fn discover_all(
    project_root: Option<&Path>,
    extra_paths: &[PathBuf],
) -> Vec<SkillMeta> {
    let mut skills: Vec<SkillMeta> = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    // 1. Bundled skills (highest priority)
    for skill in bundled::user_invocable_skills() {
        seen_names.insert(skill.name.to_string());
        for alias in skill.aliases {
            seen_names.insert(alias.to_string());
        }
        skills.push(SkillMeta {
            name: skill.name.to_string(),
            description: skill.description.to_string(),
            path: None,
            bundled: true,
            aliases: skill.aliases.iter().map(|s| s.to_string()).collect(),
            allowed_tools: skill.allowed_tools.map(|t| t.iter().map(|s| s.to_string()).collect()),
            argument_hint: skill.argument_hint.map(|s| s.to_string()),
            format: SkillFormat::Bundled,
        });
    }

    // 2. Project-level directories
    if let Some(root) = project_root {
        for dir in COMMANDS_DIRS {
            scan_claude_code_dir(&root.join(dir), &mut skills, &mut seen_names);
        }
        for dir in SKILLS_DIRS {
            scan_skills_dir(&root.join(dir), &mut skills, &mut seen_names);
        }
    }

    // 3. Home-level directories
    if let Some(home) = dirs::home_dir() {
        for dir in COMMANDS_DIRS {
            scan_claude_code_dir(&home.join(dir), &mut skills, &mut seen_names);
        }
        for dir in SKILLS_DIRS {
            scan_skills_dir(&home.join(dir), &mut skills, &mut seen_names);
        }
    }

    // 4. Extra paths
    for path in extra_paths {
        scan_claude_code_dir(path, &mut skills, &mut seen_names);
        scan_skills_dir(path, &mut skills, &mut seen_names);
    }

    skills
}

/// Scan a commands format directory: `dir/*.md`
fn scan_claude_code_dir(
    dir: &Path,
    skills: &mut Vec<SkillMeta>,
    seen: &mut HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        if name.is_empty() || seen.contains(&name.to_lowercase()) {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let (fm, body) = parse_frontmatter(&content);
        let description = fm
            .get("description")
            .cloned()
            .unwrap_or_else(|| extract_description(&body));

        let allowed_tools = fm.get("allowed-tools").map(|v| {
            v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        });

        seen.insert(name.to_lowercase());
        skills.push(SkillMeta {
            name: name.clone(),
            description,
            path: Some(path.display().to_string()),
            bundled: false,
            aliases: vec![],
            allowed_tools,
            argument_hint: fm.get("argument-hint").cloned(),
            format: SkillFormat::Commands,
        });
    }
}

/// Scan a skills format directory: `dir/<name>/SKILL.md`
fn scan_skills_dir(
    dir: &Path,
    skills: &mut Vec<SkillMeta>,
    seen: &mut HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let skill_file = path.join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }

        let content = match std::fs::read_to_string(&skill_file) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let (fm, body) = parse_frontmatter(&content);

        // Skills format requires name in frontmatter
        let name = fm
            .get("name")
            .cloned()
            .unwrap_or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string()
            });

        if name.is_empty() || seen.contains(&name.to_lowercase()) {
            continue;
        }

        let description = fm
            .get("description")
            .cloned()
            .unwrap_or_else(|| extract_description(&body));

        seen.insert(name.to_lowercase());
        skills.push(SkillMeta {
            name,
            description,
            path: Some(skill_file.display().to_string()),
            bundled: false,
            aliases: vec![],
            allowed_tools: None,
            argument_hint: None,
            format: SkillFormat::Skills,
        });
    }
}

/// Load a skill from disk by name.
/// Searches bundled first, then project/home directories.
pub fn load_skill(
    name: &str,
    project_root: Option<&Path>,
    extra_paths: &[PathBuf],
) -> Option<LoadedSkill> {
    let lower = name.to_lowercase();

    // 1. Check bundled
    if let Some(bundled) = bundled::find_bundled_skill(&lower) {
        return Some(bundled::load_bundled(bundled, None));
    }

    // 2. Search directories
    let search_dirs = build_search_dirs(project_root, extra_paths);

    for dir in &search_dirs {
        // Commands format: dir/<name>.md
        let cc_path = dir.join(format!("{}.md", name));
        if cc_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&cc_path) {
                let (fm, body) = parse_frontmatter(&content);
                let description = fm.get("description").cloned().unwrap_or_else(|| extract_description(&body));
                return Some(LoadedSkill {
                    meta: SkillMeta {
                        name: name.to_string(),
                        description,
                        path: Some(cc_path.display().to_string()),
                        bundled: false,
                        aliases: vec![],
                        allowed_tools: fm.get("allowed-tools").map(|v| {
                            v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                        }),
                        argument_hint: fm.get("argument-hint").cloned(),
                        format: SkillFormat::Commands,
                    },
                    content: body,
                });
            }
        }

        // Skills format: dir/<name>/SKILL.md
        let oc_path = dir.join(name).join("SKILL.md");
        if oc_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&oc_path) {
                let (fm, body) = parse_frontmatter(&content);
                let description = fm.get("description").cloned().unwrap_or_else(|| extract_description(&body));
                return Some(LoadedSkill {
                    meta: SkillMeta {
                        name: name.to_string(),
                        description,
                        path: Some(oc_path.display().to_string()),
                        bundled: false,
                        aliases: vec![],
                        allowed_tools: None,
                        argument_hint: None,
                        format: SkillFormat::Skills,
                    },
                    content: body,
                });
            }
        }
    }

    None
}

/// Build the list of directories to search.
fn build_search_dirs(project_root: Option<&Path>, extra_paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(root) = project_root {
        for d in COMMANDS_DIRS.iter().chain(SKILLS_DIRS.iter()) {
            dirs.push(root.join(d));
        }
    }

    if let Some(home) = dirs::home_dir() {
        for d in COMMANDS_DIRS.iter().chain(SKILLS_DIRS.iter()) {
            dirs.push(home.join(d));
        }
    }

    dirs.extend_from_slice(extra_paths);
    dirs
}

/// Format skill list for display.
pub fn format_skill_list(skills: &[SkillMeta]) -> String {
    if skills.is_empty() {
        return "No skills available.".to_string();
    }

    let mut lines = Vec::new();
    lines.push("Available skills:".to_string());

    for skill in skills {
        let tag = if skill.bundled { " [bundled]" } else { "" };
        let hint = skill
            .argument_hint
            .as_deref()
            .map(|h| format!(" {}", h))
            .unwrap_or_default();
        lines.push(format!(
            "  {}{} — {}{}",
            skill.name, hint, skill.description, tag
        ));
        if !skill.aliases.is_empty() {
            lines.push(format!("    aliases: {}", skill.aliases.join(", ")));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_discover_bundled() {
        let skills = discover_all(None, &[]);
        assert!(!skills.is_empty());
        assert!(skills.iter().any(|s| s.name == "simplify"));
        assert!(skills.iter().any(|s| s.name == "debug"));
        assert!(skills.iter().any(|s| s.name == "commit"));
    }

    #[test]
    fn test_discover_claude_code_format() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd_dir = tmp.path().join(".claude/commands");
        fs::create_dir_all(&cmd_dir).unwrap();
        fs::write(cmd_dir.join("my-skill.md"), "---\ndescription: My custom skill\n---\n\nDo $ARGUMENTS please.").unwrap();

        let skills = discover_all(Some(tmp.path()), &[]);
        let custom = skills.iter().find(|s| s.name == "my-skill");
        assert!(custom.is_some(), "Should discover commands format skill");
        assert_eq!(custom.unwrap().description, "My custom skill");
        assert_eq!(custom.unwrap().format, SkillFormat::Commands);
    }

    #[test]
    fn test_discover_skills_format() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".claude/skills/my-oc-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "---\nname: my-oc-skill\ndescription: Skills format skill\n---\n\n# Skill content").unwrap();

        let skills = discover_all(Some(tmp.path()), &[]);
        let custom = skills.iter().find(|s| s.name == "my-oc-skill");
        assert!(custom.is_some(), "Should discover skills format skill");
        assert_eq!(custom.unwrap().format, SkillFormat::Skills);
    }

    #[test]
    fn test_bundled_takes_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd_dir = tmp.path().join(".claude/commands");
        fs::create_dir_all(&cmd_dir).unwrap();
        // Create a disk skill with same name as bundled
        fs::write(cmd_dir.join("simplify.md"), "# Overridden simplify").unwrap();

        let skills = discover_all(Some(tmp.path()), &[]);
        let simplify = skills.iter().find(|s| s.name == "simplify").unwrap();
        assert!(simplify.bundled, "Bundled should take precedence over disk");
    }

    #[test]
    fn test_load_bundled_skill() {
        let loaded = load_skill("debug", None, &[]);
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert!(loaded.meta.bundled);
        assert!(loaded.content.contains("$ARGUMENTS"));
    }

    #[test]
    fn test_load_disk_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd_dir = tmp.path().join(".claude/commands");
        fs::create_dir_all(&cmd_dir).unwrap();
        fs::write(cmd_dir.join("deploy.md"), "---\ndescription: Deploy to prod\n---\n\nRun deploy for $ARGUMENTS").unwrap();

        let loaded = load_skill("deploy", Some(tmp.path()), &[]);
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert!(!loaded.meta.bundled);
        let expanded = loaded.expand(Some("staging"));
        assert!(expanded.contains("Run deploy for staging"));
    }

    #[test]
    fn test_load_from_extra_path() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("custom-skill.md"), "Do custom things").unwrap();

        let loaded = load_skill("custom-skill", None, &[tmp.path().to_path_buf()]);
        assert!(loaded.is_some());
    }

    #[test]
    fn test_real_claude_commands() {
        // Check if the user's actual ~/.claude/commands/ has skills
        let home_cmds = dirs::home_dir().map(|h| h.join(".claude/commands"));
        if let Some(dir) = home_cmds {
            if dir.exists() {
                let skills = discover_all(None, &[]);
                let disk_skills: Vec<_> = skills.iter().filter(|s| !s.bundled).collect();
                println!("Found {} disk skills from ~/.claude/commands/", disk_skills.len());
                for s in &disk_skills {
                    println!("  {} — {} ({:?})", s.name, s.description, s.format);
                }
            }
        }
    }

    #[test]
    fn test_format_skill_list() {
        let skills = discover_all(None, &[]);
        let formatted = format_skill_list(&skills);
        assert!(formatted.contains("Available skills:"));
        assert!(formatted.contains("simplify"));
        assert!(formatted.contains("[bundled]"));
    }
}
