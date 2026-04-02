//! # Phase 4 Stress Test — Skills System
//!
//! Tests skill discovery, loading, expansion, bundled skills, disk skills
//! in both Claude Code and OpenCode formats, and real user skill compatibility.
//!
//! ```bash
//! cargo run --example phase4_stress_test --release
//! ```

use cersei::prelude::*;
use std::sync::Arc;

fn main() {
    let mut passed = 0u32;
    let mut failed = 0u32;

    macro_rules! check {
        ($name:expr, $cond:expr) => {
            if $cond { passed += 1; println!("  \x1b[32m✓\x1b[0m {}", $name); }
            else { failed += 1; println!("  \x1b[31m✗\x1b[0m {}", $name); }
        };
    }

    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║  Phase 4 — Skills System Stress Test              ║");
    println!("╚══════════════════════════════════════════════════╝\n");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = ToolContext {
        working_dir: std::env::temp_dir(),
        session_id: "phase4".into(),
        permissions: Arc::new(AllowAll),
        cost_tracker: Arc::new(CostTracker::new()),
        mcp_manager: None,
        extensions: Extensions::default(),
    };

    // ── 1. Bundled Skills ────────────────────────────────────────────────
    println!("  1. Bundled Skills");
    println!("  ─────────────────");
    {
        use cersei_tools::skills::bundled::*;

        let all = user_invocable_skills();
        check!(&format!("{} bundled skills", all.len()), all.len() >= 7);

        // Find by name
        check!("find 'simplify'", find_bundled_skill("simplify").is_some());
        check!("find 'debug'", find_bundled_skill("debug").is_some());
        check!("find 'commit'", find_bundled_skill("commit").is_some());
        check!("find 'verify'", find_bundled_skill("verify").is_some());
        check!("find 'remember'", find_bundled_skill("remember").is_some());
        check!("find 'stuck'", find_bundled_skill("stuck").is_some());
        check!("find 'loop'", find_bundled_skill("loop").is_some());

        // Find by alias
        check!("alias 'mem' → remember", find_bundled_skill("mem").unwrap().name == "remember");
        check!("alias 'diagnose' → debug", find_bundled_skill("diagnose").unwrap().name == "debug");
        check!("alias 'help-me' → stuck", find_bundled_skill("help-me").unwrap().name == "stuck");
        check!("alias 'check' → verify", find_bundled_skill("check").unwrap().name == "verify");

        // Case insensitive
        check!("case insensitive", find_bundled_skill("SIMPLIFY").is_some());
        check!("not found", find_bundled_skill("nonexistent").is_none());

        // Allowed tools
        let debug = find_bundled_skill("debug").unwrap();
        check!("debug has allowed_tools", debug.allowed_tools.is_some());
        check!("debug allows Read", debug.allowed_tools.unwrap().contains(&"Read"));

        let simplify = find_bundled_skill("simplify").unwrap();
        check!("simplify has no tool restriction", simplify.allowed_tools.is_none());

        // Load and expand
        let loaded = load_bundled(debug, None);
        let expanded = loaded.expand(Some("tests are flaky"));
        check!("expand replaces $ARGUMENTS", expanded.contains("tests are flaky"));
        check!("expand removes $ARGUMENTS marker", !expanded.contains("$ARGUMENTS"));
        println!();
    }

    // ── 2. Disk Skills (Claude Code Format) ──────────────────────────────
    println!("  2. Claude Code Format (.claude/commands/*.md)");
    println!("  ──────────────────────────────────────────────");
    {
        use cersei_tools::skills::discovery::*;

        let tmp = tempfile::tempdir().unwrap();
        let cmd_dir = tmp.path().join(".claude/commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();

        // Create test skills
        std::fs::write(cmd_dir.join("deploy.md"), "\
---
description: Deploy the application
argument-hint: <environment>
allowed-tools: Bash, Read
---

Deploy $ARGUMENTS to the target environment.
1. Run tests first
2. Build the release
3. Deploy$ARGUMENTS_SUFFIX
").unwrap();

        std::fs::write(cmd_dir.join("review.md"), "\
# Code Review

Review the changes in this PR and provide feedback.
Focus on: correctness, performance, readability.
").unwrap();

        std::fs::write(cmd_dir.join("no-frontmatter.md"), "\
Just a plain skill with no YAML frontmatter.
It should still be discoverable.
").unwrap();

        let skills = discover_all(Some(tmp.path()), &[]);
        let deploy = skills.iter().find(|s| s.name == "deploy");
        check!("discover deploy.md", deploy.is_some());
        check!("deploy has description from FM", deploy.unwrap().description == "Deploy the application");
        check!("deploy has allowed_tools", deploy.unwrap().allowed_tools.is_some());

        let review = skills.iter().find(|s| s.name == "review");
        check!("discover review.md", review.is_some());
        check!("review description from first line", review.unwrap().description.contains("Code Review"));

        let plain = skills.iter().find(|s| s.name == "no-frontmatter");
        check!("discover no-frontmatter.md", plain.is_some());

        // Load and expand
        let loaded = load_skill("deploy", Some(tmp.path()), &[]);
        check!("load deploy", loaded.is_some());
        let loaded = loaded.unwrap();
        let expanded = loaded.expand(Some("production"));
        check!("expand $ARGUMENTS", expanded.contains("Deploy production"));
        check!("expand $ARGUMENTS_SUFFIX", expanded.contains(": production"));
        check!("no leftover markers", !expanded.contains("$ARGUMENTS"));
        println!();
    }

    // ── 3. Disk Skills (OpenCode Format) ─────────────────────────────────
    println!("  3. OpenCode Format (.claude/skills/<name>/SKILL.md)");
    println!("  ───────────────────────────────────────────────────");
    {
        use cersei_tools::skills::discovery::*;

        let tmp = tempfile::tempdir().unwrap();

        // Create OpenCode format skill
        let skill_dir = tmp.path().join(".claude/skills/cloudflare");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "\
---
name: cloudflare
description: Cloudflare platform development skill
references:
  - workers
  - pages
---

# Cloudflare Platform

Use this skill for any Cloudflare development.

## Workers
Deploy serverless functions to Cloudflare Workers.
").unwrap();

        // Create agents-sdk skill
        let skill_dir2 = tmp.path().join(".claude/skills/agents-sdk");
        std::fs::create_dir_all(&skill_dir2).unwrap();
        std::fs::write(skill_dir2.join("SKILL.md"), "\
---
name: agents-sdk
description: Build AI agents with the Agents SDK
---

# Agents SDK Skill

Build durable AI agents that survive restarts.
").unwrap();

        let skills = discover_all(Some(tmp.path()), &[]);
        let cf = skills.iter().find(|s| s.name == "cloudflare");
        check!("discover cloudflare SKILL.md", cf.is_some());
        check!("cloudflare format is OpenCode", cf.unwrap().format == cersei_tools::skills::SkillFormat::OpenCode);

        let agents = skills.iter().find(|s| s.name == "agents-sdk");
        check!("discover agents-sdk SKILL.md", agents.is_some());

        // Load
        let loaded = load_skill("cloudflare", Some(tmp.path()), &[]);
        check!("load cloudflare", loaded.is_some());
        check!("content has Workers section", loaded.unwrap().content.contains("Workers"));
        println!();
    }

    // ── 4. Precedence & Deduplication ────────────────────────────────────
    println!("  4. Precedence & Deduplication");
    println!("  ─────────────────────────────");
    {
        use cersei_tools::skills::discovery::*;

        let tmp = tempfile::tempdir().unwrap();
        let cmd_dir = tmp.path().join(".claude/commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();

        // Create a disk skill with same name as bundled
        std::fs::write(cmd_dir.join("simplify.md"), "# Overridden simplify\nDisk version.").unwrap();

        let skills = discover_all(Some(tmp.path()), &[]);
        let simplify = skills.iter().find(|s| s.name == "simplify").unwrap();
        check!("bundled takes precedence", simplify.bundled);

        // No duplicate
        let simplify_count = skills.iter().filter(|s| s.name == "simplify").count();
        check!("no duplicate simplify", simplify_count == 1);
        println!();
    }

    // ── 5. SkillTool Integration ─────────────────────────────────────────
    println!("  5. SkillTool (via Agent)");
    println!("  ────────────────────────");
    rt.block_on(async {
        let tmp = tempfile::tempdir().unwrap();
        let cmd_dir = tmp.path().join(".claude/commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("greet.md"), "Say hello to $ARGUMENTS warmly.").unwrap();

        let tool = cersei_tools::skill_tool::SkillTool::new().with_project_root(tmp.path());
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            ..ctx.clone()
        };

        // List
        let r = tool.execute(serde_json::json!({"skill": "list"}), &ctx).await;
        check!("SkillTool list works", r.content.contains("Available skills:"));
        check!("List includes bundled", r.content.contains("simplify"));
        check!("List includes disk skill", r.content.contains("greet"));

        // Load bundled
        let r = tool.execute(serde_json::json!({"skill": "commit"}), &ctx).await;
        check!("Load bundled 'commit'", !r.is_error && r.content.contains("git"));

        // Load disk
        let r = tool.execute(serde_json::json!({"skill": "greet", "args": "world"}), &ctx).await;
        check!("Load disk 'greet'", !r.is_error);
        check!("Expand $ARGUMENTS", r.content.contains("Say hello to world"));

        // Not found with suggestion
        let r = tool.execute(serde_json::json!({"skill": "simp"}), &ctx).await;
        check!("Not found gives suggestion", r.is_error && r.content.contains("Did you mean"));
    });
    println!();

    // ── 6. Real User Skills ──────────────────────────────────────────────
    println!("  6. Real User Skills (~/.claude/commands/)");
    println!("  ─────────────────────────────────────────");
    {
        use cersei_tools::skills::discovery::*;

        let home_cmds = dirs::home_dir().map(|h| h.join(".claude/commands"));
        if let Some(ref dir) = home_cmds {
            if dir.exists() {
                let skills = discover_all(None, &[]);
                let disk_skills: Vec<_> = skills.iter().filter(|s| !s.bundled).collect();
                check!(&format!("Found {} user skills", disk_skills.len()), !disk_skills.is_empty());

                for s in &disk_skills {
                    println!("    {} — {} [{:?}]", s.name, &s.description[..s.description.len().min(50)], s.format);
                }

                // Try loading the design skill specifically
                let loaded = load_skill("design", None, &[]);
                if let Some(loaded) = loaded {
                    check!("Load 'design' skill", true);
                    check!(&format!("design.md is {} chars", loaded.content.len()), loaded.content.len() > 100);
                    check!("design has real content", loaded.content.contains("Design") || loaded.content.contains("design"));
                } else {
                    println!("    (design skill not found — skipping)");
                }
            } else {
                println!("    No ~/.claude/commands/ directory — skipping real skill tests");
            }
        }
        println!();
    }

    // ── 7. Format Skill List Output ──────────────────────────────────────
    println!("  7. Skill List Formatting");
    println!("  ────────────────────────");
    {
        use cersei_tools::skills::discovery::*;

        let skills = discover_all(None, &[]);
        let formatted = format_skill_list(&skills);

        check!("Formatted list starts with header", formatted.starts_with("Available skills:"));
        check!("Contains [bundled] tag", formatted.contains("[bundled]"));
        check!("Contains aliases", formatted.contains("aliases:") || skills.iter().all(|s| s.aliases.is_empty()));

        // Check formatting structure
        let lines: Vec<&str> = formatted.lines().collect();
        check!(&format!("Has {} lines for {} skills", lines.len(), skills.len()), lines.len() > skills.len());
        println!();
    }

    // ── Summary ──────────────────────────────────────────────────────────
    println!("╔══════════════════════════════════════════════════╗");
    if failed == 0 {
        println!("║  \x1b[32mALL {} CHECKS PASSED\x1b[0m                              ║", passed);
    } else {
        println!("║  \x1b[31m{} PASSED, {} FAILED\x1b[0m                               ║", passed, failed);
    }
    println!("╚══════════════════════════════════════════════════╝\n");
    if failed > 0 { std::process::exit(1); }
}
