//! Bundled skills: compile-time skill definitions shipped with Cersei.

use super::{LoadedSkill, SkillFormat, SkillMeta};

/// A bundled skill definition.
pub struct BundledSkill {
    pub name: &'static str,
    pub description: &'static str,
    pub aliases: &'static [&'static str],
    pub when_to_use: Option<&'static str>,
    pub argument_hint: Option<&'static str>,
    pub prompt_template: &'static str,
    pub allowed_tools: Option<&'static [&'static str]>,
    pub user_invocable: bool,
}

/// All bundled skills.
pub const BUNDLED_SKILLS: &[BundledSkill] = &[
    BundledSkill {
        name: "simplify",
        description: "Review changed code for reuse, quality, and efficiency, then fix any issues found.",
        aliases: &[],
        when_to_use: Some("After editing multiple files or completing a feature"),
        argument_hint: None,
        prompt_template: "Review the code I've changed in this session for:\n\
            1. Opportunities to reuse existing functions/utilities instead of duplicating\n\
            2. Code quality issues (naming, structure, error handling)\n\
            3. Performance improvements\n\
            4. Unnecessary complexity that can be simplified\n\n\
            Fix any issues you find. Don't add features or refactor beyond what's needed.$ARGUMENTS_SUFFIX",
        allowed_tools: None,
        user_invocable: true,
    },
    BundledSkill {
        name: "remember",
        description: "Save information to persistent memory for future sessions.",
        aliases: &["mem", "save"],
        when_to_use: Some("When the user asks you to remember something"),
        argument_hint: Some("<what to remember>"),
        prompt_template: "Save the following to memory: $ARGUMENTS",
        allowed_tools: Some(&["Read", "Write", "Edit", "Glob"]),
        user_invocable: true,
    },
    BundledSkill {
        name: "debug",
        description: "Investigate and diagnose a bug or issue.",
        aliases: &["diagnose"],
        when_to_use: Some("When something is broken or behaving unexpectedly"),
        argument_hint: Some("<description of the issue>"),
        prompt_template: "Investigate this issue: $ARGUMENTS\n\n\
            1. Search for relevant code and error messages\n\
            2. Identify the root cause\n\
            3. Suggest a fix with code changes\n\
            4. Explain why it was broken",
        allowed_tools: Some(&["Read", "Grep", "Glob"]),
        user_invocable: true,
    },
    BundledSkill {
        name: "stuck",
        description: "Get unstuck when you're blocked on a problem.",
        aliases: &["help-me", "unblock"],
        when_to_use: Some("When the agent is going in circles or can't make progress"),
        argument_hint: Some("<what you're stuck on>"),
        prompt_template: "I'm stuck on: $ARGUMENTS\n\n\
            Take a step back and think about this differently:\n\
            1. What have I already tried?\n\
            2. What assumptions am I making that might be wrong?\n\
            3. Is there a simpler approach?\n\
            4. Should I ask the user for clarification?",
        allowed_tools: None,
        user_invocable: true,
    },
    BundledSkill {
        name: "verify",
        description: "Verify that recent changes work correctly end-to-end.",
        aliases: &["check", "validate"],
        when_to_use: Some("After making changes, to confirm they work"),
        argument_hint: None,
        prompt_template: "Verify the recent changes work correctly:\n\
            1. Identify what was changed\n\
            2. Run relevant tests or checks\n\
            3. Verify the feature/fix works as intended\n\
            4. Check for regressions$ARGUMENTS_SUFFIX",
        allowed_tools: None,
        user_invocable: true,
    },
    BundledSkill {
        name: "commit",
        description: "Create a git commit with a well-crafted message.",
        aliases: &[],
        when_to_use: Some("When the user asks to commit changes"),
        argument_hint: Some("<optional commit message hint>"),
        prompt_template: "Create a git commit for the current changes.\n\n\
            1. Run `git status` and `git diff --staged` to see what's changed\n\
            2. If nothing is staged, stage the relevant files\n\
            3. Write a concise commit message that explains the 'why'\n\
            4. Create the commit$ARGUMENTS_SUFFIX",
        allowed_tools: None,
        user_invocable: true,
    },
    BundledSkill {
        name: "loop",
        description: "Run a prompt or slash command on a recurring interval.",
        aliases: &[],
        when_to_use: Some("When the user wants to poll or repeat a task"),
        argument_hint: Some("<interval> <command>"),
        prompt_template: "Set up a recurring task: $ARGUMENTS\n\n\
            Use CronCreate to schedule this. Parse the interval from the arguments.",
        allowed_tools: Some(&["CronCreate", "CronList"]),
        user_invocable: true,
    },
];

/// Find a bundled skill by name or alias (case-insensitive).
pub fn find_bundled_skill(name: &str) -> Option<&'static BundledSkill> {
    let lower = name.to_lowercase();
    BUNDLED_SKILLS
        .iter()
        .find(|s| s.name == lower || s.aliases.iter().any(|a| *a == lower))
}

/// Get all user-invocable bundled skills.
pub fn user_invocable_skills() -> Vec<&'static BundledSkill> {
    BUNDLED_SKILLS.iter().filter(|s| s.user_invocable).collect()
}

/// Convert a bundled skill to a LoadedSkill.
pub fn load_bundled(skill: &BundledSkill, _args: Option<&str>) -> LoadedSkill {
    LoadedSkill {
        meta: SkillMeta {
            name: skill.name.to_string(),
            description: skill.description.to_string(),
            path: None,
            bundled: true,
            aliases: skill.aliases.iter().map(|s| s.to_string()).collect(),
            allowed_tools: skill
                .allowed_tools
                .map(|t| t.iter().map(|s| s.to_string()).collect()),
            argument_hint: skill.argument_hint.map(|s| s.to_string()),
            format: SkillFormat::Bundled,
        },
        content: skill.prompt_template.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_by_name() {
        assert!(find_bundled_skill("simplify").is_some());
        assert!(find_bundled_skill("debug").is_some());
        assert!(find_bundled_skill("commit").is_some());
    }

    #[test]
    fn test_find_by_alias() {
        assert!(find_bundled_skill("mem").is_some());
        assert_eq!(find_bundled_skill("mem").unwrap().name, "remember");
        assert!(find_bundled_skill("diagnose").is_some());
        assert_eq!(find_bundled_skill("diagnose").unwrap().name, "debug");
        assert!(find_bundled_skill("help-me").is_some());
    }

    #[test]
    fn test_case_insensitive() {
        assert!(find_bundled_skill("SIMPLIFY").is_some());
        assert!(find_bundled_skill("Debug").is_some());
    }

    #[test]
    fn test_not_found() {
        assert!(find_bundled_skill("nonexistent").is_none());
    }

    #[test]
    fn test_user_invocable() {
        let invocable = user_invocable_skills();
        assert!(invocable.len() >= 7);
        assert!(invocable.iter().all(|s| s.user_invocable));
    }

    #[test]
    fn test_load_bundled_expand() {
        let skill = find_bundled_skill("debug").unwrap();
        let loaded = load_bundled(skill, Some("the tests are flaky"));
        let expanded = loaded.expand(Some("the tests are flaky"));
        assert!(expanded.contains("the tests are flaky"));
        assert!(!expanded.contains("$ARGUMENTS"));
    }
}
