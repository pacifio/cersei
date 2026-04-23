//! Skill tool: load and execute skill prompt templates.
//! Supports:
//! - `skill="list"` — list all available skills
//! - `skill="<name>" args="<arguments>"` — load and expand a skill

use super::*;
use crate::skills::discovery;
use serde::Deserialize;

pub struct SkillTool {
    /// Project root for skill discovery.
    project_root: Option<std::path::PathBuf>,
    /// Extra directories to search for skills.
    extra_paths: Vec<std::path::PathBuf>,
}

impl SkillTool {
    pub fn new() -> Self {
        Self {
            project_root: None,
            extra_paths: Vec::new(),
        }
    }

    pub fn with_project_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.project_root = Some(root.into());
        self
    }

    pub fn with_extra_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.extra_paths.push(path.into());
        self
    }

    pub fn with_extra_paths(mut self, paths: Vec<std::path::PathBuf>) -> Self {
        self.extra_paths.extend(paths);
        self
    }
}

impl Default for SkillTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        "Load and execute a skill (prompt template). Use skill='list' to see available skills."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Custom
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "Skill name, or 'list' to show available skills"
                },
                "args": {
                    "type": "string",
                    "description": "Arguments to pass to the skill (replaces $ARGUMENTS)"
                }
            },
            "required": ["skill"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            skill: String,
            args: Option<String>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        // List mode
        if input.skill == "list" {
            let project_root = self
                .project_root
                .as_deref()
                .or_else(|| Some(ctx.working_dir.as_path()));
            let skills = discovery::discover_all(project_root, &self.extra_paths);
            return ToolResult::success(discovery::format_skill_list(&skills));
        }

        // Load mode
        let project_root = self
            .project_root
            .as_deref()
            .or_else(|| Some(ctx.working_dir.as_path()));
        let loaded = discovery::load_skill(&input.skill, project_root, &self.extra_paths);

        match loaded {
            Some(skill) => {
                let expanded = skill.expand(input.args.as_deref());

                // Include metadata in the result
                let mut meta = serde_json::json!({
                    "skill_name": skill.meta.name,
                    "format": format!("{:?}", skill.meta.format),
                    "bundled": skill.meta.bundled,
                });
                if let Some(tools) = &skill.meta.allowed_tools {
                    meta["allowed_tools"] = serde_json::json!(tools);
                }

                ToolResult::success(expanded).with_metadata(meta)
            }
            None => {
                // Suggest similar skills
                let all = discovery::discover_all(project_root, &self.extra_paths);
                let suggestions: Vec<&str> = all
                    .iter()
                    .filter(|s| s.name.contains(&input.skill) || input.skill.contains(&s.name))
                    .map(|s| s.name.as_str())
                    .take(5)
                    .collect();

                let mut msg = format!("Skill '{}' not found.", input.skill);
                if !suggestions.is_empty() {
                    msg.push_str(&format!("\n\nDid you mean: {}?", suggestions.join(", ")));
                }
                msg.push_str("\n\nUse skill='list' to see all available skills.");
                ToolResult::error(msg)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "skill-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_skill_list() {
        let tool = SkillTool::new();
        let r = tool
            .execute(serde_json::json!({"skill": "list"}), &test_ctx())
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("Available skills:"));
        assert!(r.content.contains("simplify"));
        assert!(r.content.contains("[bundled]"));
    }

    #[tokio::test]
    async fn test_skill_load_bundled() {
        let tool = SkillTool::new();
        let r = tool
            .execute(
                serde_json::json!({
                    "skill": "debug",
                    "args": "the login page crashes"
                }),
                &test_ctx(),
            )
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("the login page crashes"));
        assert!(!r.content.contains("$ARGUMENTS"));
        assert!(r.metadata.is_some());
        assert_eq!(r.metadata.as_ref().unwrap()["bundled"], true);
    }

    #[tokio::test]
    async fn test_skill_load_by_alias() {
        let tool = SkillTool::new();
        let r = tool
            .execute(
                serde_json::json!({"skill": "diagnose", "args": "memory leak"}),
                &test_ctx(),
            )
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("memory leak"));
    }

    #[tokio::test]
    async fn test_skill_not_found() {
        let tool = SkillTool::new();
        let r = tool
            .execute(serde_json::json!({"skill": "nonexistent"}), &test_ctx())
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("not found"));
    }

    #[tokio::test]
    async fn test_skill_load_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd_dir = tmp.path().join(".claude/commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(
            cmd_dir.join("my-deploy.md"),
            "---\ndescription: Deploy the app\n---\n\nDeploy $ARGUMENTS to production.",
        )
        .unwrap();

        let tool = SkillTool::new().with_project_root(tmp.path());
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            ..test_ctx()
        };

        // List should include it
        let r = tool
            .execute(serde_json::json!({"skill": "list"}), &ctx)
            .await;
        assert!(r.content.contains("my-deploy"));

        // Load and expand
        let r = tool
            .execute(
                serde_json::json!({"skill": "my-deploy", "args": "v2.0"}),
                &ctx,
            )
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("Deploy v2.0 to production"));
        assert_eq!(r.metadata.as_ref().unwrap()["format"], "Commands");
    }

    #[tokio::test]
    async fn test_skill_skills_format() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".claude/skills/aws-deploy");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: aws-deploy\ndescription: Deploy to AWS\n---\n\n# AWS Deploy\n\nUse CDK to deploy.",
        )
        .unwrap();

        let tool = SkillTool::new().with_project_root(tmp.path());
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            ..test_ctx()
        };

        let r = tool
            .execute(serde_json::json!({"skill": "aws-deploy"}), &ctx)
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("CDK"));
        assert_eq!(r.metadata.as_ref().unwrap()["format"], "Skills");
    }

    #[tokio::test]
    async fn test_real_user_skills() {
        // Test compatibility with actual ~/.claude/commands/ skills
        let tool = SkillTool::new();
        let r = tool
            .execute(serde_json::json!({"skill": "list"}), &test_ctx())
            .await;
        // Should at least have bundled skills
        assert!(r.content.contains("simplify"));

        // Try loading "design" if it exists (from ~/.claude/commands/design.md)
        let r = tool
            .execute(serde_json::json!({"skill": "design"}), &test_ctx())
            .await;
        if !r.is_error {
            println!("Loaded real user skill 'design': {} chars", r.content.len());
            assert!(r.content.len() > 100); // design.md is substantial
        }
    }
}
