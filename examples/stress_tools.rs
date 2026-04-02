//! # Phase 2 Stress Test — All Tools Validation
//!
//! Tests every tool that can run without external APIs or git repos.
//! Validates schemas, execution, and the tool registry.
//!
//! ```bash
//! cargo run --example phase2_stress_test --release
//! ```

use cersei::prelude::*;
use std::sync::Arc;

fn ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext {
        working_dir: dir.to_path_buf(),
        session_id: "phase2-test".into(),
        permissions: Arc::new(AllowAll),
        cost_tracker: Arc::new(CostTracker::new()),
        mcp_manager: None,
        extensions: Extensions::default(),
    }
}

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async { run().await });
}

async fn run() {
    let mut passed = 0u32;
    let mut failed = 0u32;

    macro_rules! check {
        ($name:expr, $cond:expr) => {
            if $cond {
                passed += 1;
                println!("  \x1b[32m✓\x1b[0m {}", $name);
            } else {
                failed += 1;
                println!("  \x1b[31m✗\x1b[0m {}", $name);
            }
        };
    }

    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║  Phase 2 — All Tools Stress Test                 ║");
    println!("╚══════════════════════════════════════════════════╝\n");

    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());

    // ── 1. Tool Registry ─────────────────────────────────────────────────
    println!("  1. Tool Registry");
    println!("  ────────────────");
    {
        let all = cersei::tools::all();
        let fs = cersei::tools::filesystem();
        let sh = cersei::tools::shell();
        let web = cersei::tools::web();
        let plan = cersei::tools::planning();
        let sched = cersei::tools::scheduling();
        let orch = cersei::tools::orchestration();

        check!(&format!("all() returns {} tools", all.len()), all.len() >= 24);
        check!(&format!("filesystem() = {} tools", fs.len()), fs.len() == 6);
        check!(&format!("shell() = {} tools", sh.len()), sh.len() == 2);
        check!(&format!("web() = {} tools", web.len()), web.len() == 2);
        check!(&format!("planning() = {} tools", plan.len()), plan.len() == 3);
        check!(&format!("scheduling() = {} tools", sched.len()), sched.len() == 5);
        check!(&format!("orchestration() = {} tools", orch.len()), orch.len() == 3);
        check!("none() is empty", cersei::tools::none().is_empty());

        // All tools have valid schemas
        let mut all_valid = true;
        for tool in &all {
            let schema = tool.input_schema();
            if !schema.is_object() {
                println!("    BAD SCHEMA: {} → {:?}", tool.name(), schema);
                all_valid = false;
            }
        }
        check!("All tools have valid JSON schemas", all_valid);

        // All tools have unique names
        let names: Vec<String> = all.iter().map(|t| t.name().to_string()).collect();
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        check!(&format!("All {} tool names unique", names.len()), names.len() == unique.len());

        // Print tool inventory
        println!("\n    Tool inventory:");
        for tool in &all {
            println!("      {:<18} {:?} / {:?}", tool.name(), tool.permission_level(), tool.category());
        }
        println!();
    }

    // ── 2. Filesystem Tools ──────────────────────────────────────────────
    println!("  2. Filesystem Tools");
    println!("  ───────────────────");
    {
        // Write a file
        let tools = cersei::tools::filesystem();
        let write_tool = tools.iter().find(|t| t.name() == "Write").unwrap();
        let r = write_tool.execute(serde_json::json!({
            "file_path": tmp.path().join("test.txt").display().to_string(),
            "content": "Hello, Phase 2!\nLine 2\nLine 3\n"
        }), &ctx).await;
        check!("Write creates file", !r.is_error);

        // Read it back
        let read_tool = tools.iter().find(|t| t.name() == "Read").unwrap();
        let r = read_tool.execute(serde_json::json!({
            "file_path": tmp.path().join("test.txt").display().to_string()
        }), &ctx).await;
        check!("Read returns content", r.content.contains("Hello, Phase 2!"));

        // Edit it
        let edit_tool = tools.iter().find(|t| t.name() == "Edit").unwrap();
        let r = edit_tool.execute(serde_json::json!({
            "file_path": tmp.path().join("test.txt").display().to_string(),
            "old_string": "Hello, Phase 2!",
            "new_string": "Hello, Cersei!"
        }), &ctx).await;
        check!("Edit replaces string", !r.is_error);

        // Verify edit
        let r = read_tool.execute(serde_json::json!({
            "file_path": tmp.path().join("test.txt").display().to_string()
        }), &ctx).await;
        check!("Edit persisted", r.content.contains("Cersei"));

        // Glob
        for i in 0..5 {
            std::fs::write(tmp.path().join(format!("mod_{}.rs", i)), format!("fn f{}() {{}}", i)).unwrap();
        }
        let glob_tool = tools.iter().find(|t| t.name() == "Glob").unwrap();
        let r = glob_tool.execute(serde_json::json!({
            "pattern": "**/*.rs",
            "path": tmp.path().display().to_string()
        }), &ctx).await;
        check!("Glob finds .rs files", r.content.contains("mod_0.rs"));

        // Grep
        let grep_tool = tools.iter().find(|t| t.name() == "Grep").unwrap();
        let r = grep_tool.execute(serde_json::json!({
            "pattern": "fn f",
            "path": tmp.path().display().to_string()
        }), &ctx).await;
        check!("Grep finds pattern", r.content.contains("fn f"));

        // NotebookEdit
        let nb_path = tmp.path().join("test.ipynb");
        std::fs::write(&nb_path, serde_json::json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {},
            "cells": [{"cell_type": "code", "source": ["x = 1\n"], "outputs": [], "metadata": {}}]
        }).to_string()).unwrap();
        let nb_tool = tools.iter().find(|t| t.name() == "NotebookEdit").unwrap();
        let r = nb_tool.execute(serde_json::json!({
            "file_path": nb_path.display().to_string(),
            "cell_index": 0,
            "new_source": "x = 42"
        }), &ctx).await;
        check!("NotebookEdit updates cell", !r.is_error);
        println!();
    }

    // ── 3. Shell Tools ───────────────────────────────────────────────────
    println!("  3. Shell Tools");
    println!("  ──────────────");
    {
        let tools = cersei::tools::shell();
        let bash = tools.iter().find(|t| t.name() == "Bash").unwrap();
        let r = bash.execute(serde_json::json!({"command": "echo 'Phase2 OK'"}), &ctx).await;
        check!("Bash executes", r.content.contains("Phase2 OK"));

        let r = bash.execute(serde_json::json!({"command": "false"}), &ctx).await;
        check!("Bash reports errors", r.is_error);
        println!();
    }

    // ── 4. Planning Tools ────────────────────────────────────────────────
    println!("  4. Planning Tools");
    println!("  ─────────────────");
    {
        cersei_tools::plan_mode::set_plan_mode(false);
        let enter = cersei_tools::plan_mode::EnterPlanModeTool;
        enter.execute(serde_json::json!({}), &ctx).await;
        check!("EnterPlanMode sets flag", cersei_tools::plan_mode::is_plan_mode());

        let exit = cersei_tools::plan_mode::ExitPlanModeTool;
        exit.execute(serde_json::json!({}), &ctx).await;
        check!("ExitPlanMode clears flag", !cersei_tools::plan_mode::is_plan_mode());

        let todo = cersei_tools::todo_write::TodoWriteTool;
        let r = todo.execute(serde_json::json!({
            "todos": [
                {"content": "Task A", "status": "in_progress", "activeForm": "Doing A"},
                {"content": "Task B", "status": "pending", "activeForm": "Doing B"},
            ]
        }), &ctx).await;
        check!("TodoWrite creates items", r.content.contains("2 items"));
        let todos = cersei_tools::todo_write::get_todos("phase2-test");
        check!("Todos persisted in registry", todos.len() == 2);
        println!();
    }

    // ── 5. Scheduling Tools ──────────────────────────────────────────────
    println!("  5. Scheduling Tools");
    println!("  ───────────────────");
    {
        cersei_tools::cron::clear_crons();
        let create = cersei_tools::cron::CronCreateTool;
        let r = create.execute(serde_json::json!({
            "schedule": "*/10 * * * *",
            "prompt": "Check build status"
        }), &ctx).await;
        check!("CronCreate succeeds", !r.is_error);

        let list = cersei_tools::cron::CronListTool;
        let r = list.execute(serde_json::json!({}), &ctx).await;
        check!("CronList shows entry", r.content.contains("Check build status"));

        let entries = cersei_tools::cron::list_crons();
        let id = entries[0].id.clone();
        let delete = cersei_tools::cron::CronDeleteTool;
        delete.execute(serde_json::json!({"id": id}), &ctx).await;
        check!("CronDelete removes entry", cersei_tools::cron::list_crons().is_empty());

        let sleep = cersei_tools::sleep::SleepTool;
        let start = std::time::Instant::now();
        sleep.execute(serde_json::json!({"duration_ms": 50}), &ctx).await;
        check!("Sleep waits correctly", start.elapsed().as_millis() >= 40);

        let trigger = cersei_tools::remote_trigger::RemoteTriggerTool;
        let r = trigger.execute(serde_json::json!({
            "target_session": "other",
            "event_type": "deploy",
            "payload": {"env": "staging"}
        }), &ctx).await;
        check!("RemoteTrigger sends", !r.is_error);
        let events = cersei_tools::remote_trigger::drain_triggers("other");
        check!("Trigger received", events.len() == 1 && events[0].event_type == "deploy");
        println!();
    }

    // ── 6. Orchestration Tools ───────────────────────────────────────────
    println!("  6. Orchestration Tools");
    println!("  ──────────────────────");
    {
        let send = cersei_tools::send_message::SendMessageTool;
        let r = send.execute(serde_json::json!({"to": "worker-1", "content": "Start task"}), &ctx).await;
        check!("SendMessage delivers", !r.is_error);
        let msgs = cersei_tools::send_message::peek_inbox("worker-1");
        check!("Inbox has message", msgs.len() == 1 && msgs[0].content == "Start task");
        let drained = cersei_tools::send_message::drain_inbox("worker-1");
        check!("Drain clears inbox", drained.len() == 1 && cersei_tools::send_message::peek_inbox("worker-1").is_empty());
        println!();
    }

    // ── 7. Config Tool ───────────────────────────────────────────────────
    println!("  7. Config & Utility Tools");
    println!("  ─────────────────────────");
    {
        let cfg = cersei_tools::config_tool::ConfigTool;
        cfg.execute(serde_json::json!({"action": "set", "key": "model", "value": "claude-opus"}), &ctx).await;
        let r = cfg.execute(serde_json::json!({"action": "get", "key": "model"}), &ctx).await;
        check!("Config set/get works", r.content.contains("claude-opus"));

        let so = cersei_tools::synthetic_output::SyntheticOutputTool;
        let r = so.execute(serde_json::json!({"data": {"status": "ok", "count": 42}}), &ctx).await;
        check!("SyntheticOutput returns JSON", r.content.contains("42"));
        check!("SyntheticOutput has metadata", r.metadata.is_some());

        let ask = cersei_tools::ask_user::AskUserQuestionTool;
        let r = ask.execute(serde_json::json!({"question": "Continue?"}), &ctx).await;
        check!("AskUser returns question", r.content.contains("Continue?"));
        println!();
    }

    // ── 8. Web Tools (schema-only, no live API) ──────────────────────────
    println!("  8. Web Tools (schema validation)");
    println!("  ────────────────────────────────");
    {
        let wf = cersei_tools::web_fetch::WebFetchTool;
        check!("WebFetch has url in schema", wf.input_schema()["properties"]["url"].is_object());
        check!("WebFetch is ReadOnly", wf.permission_level() == PermissionLevel::ReadOnly);

        let ws = cersei_tools::web_search::WebSearchTool;
        check!("WebSearch has query in schema", ws.input_schema()["properties"]["query"].is_object());
        check!("WebSearch is ReadOnly", ws.permission_level() == PermissionLevel::ReadOnly);
        println!();
    }

    // ── 9. ToolSearch ────────────────────────────────────────────────────
    println!("  9. ToolSearch");
    println!("  ─────────────");
    {
        let all = cersei::tools::all();
        let search = cersei_tools::tool_search::ToolSearchTool::new(&all);
        let r = search.execute(serde_json::json!({"query": "file"}), &ctx).await;
        check!("ToolSearch finds file tools", r.content.contains("Read") && r.content.contains("Write"));

        let r = search.execute(serde_json::json!({"query": "bash"}), &ctx).await;
        check!("ToolSearch finds Bash", r.content.contains("Bash"));

        let r = search.execute(serde_json::json!({"query": "cron"}), &ctx).await;
        check!("ToolSearch finds Cron", r.content.contains("Cron"));

        let r = search.execute(serde_json::json!({"query": "xyznonexistent"}), &ctx).await;
        check!("ToolSearch handles no results", r.content.contains("No tools found"));
        println!();
    }

    // ── 10. Benchmark: tool dispatch speed ───────────────────────────────
    println!("  10. Performance (tool dispatch speed)");
    println!("  ─────────────────────────────────────");
    {
        let tools = cersei::tools::filesystem();
        let read = tools.iter().find(|t| t.name() == "Read").unwrap();

        // Create test file
        let test_file = tmp.path().join("perf_test.txt");
        std::fs::write(&test_file, "x\n".repeat(500)).unwrap();

        let iters = 100u32;
        let start = std::time::Instant::now();
        for _ in 0..iters {
            read.execute(serde_json::json!({"file_path": test_file.display().to_string()}), &ctx).await;
        }
        let avg_us = start.elapsed().as_micros() / iters as u128;
        check!(&format!("Read avg {}μs (< 500μs target)", avg_us), avg_us < 500);

        let edit = tools.iter().find(|t| t.name() == "Edit").unwrap();
        let start = std::time::Instant::now();
        for _ in 0..iters {
            std::fs::write(&test_file, "Hello, world!\nLine 2\n").unwrap();
            edit.execute(serde_json::json!({
                "file_path": test_file.display().to_string(),
                "old_string": "Hello, world!",
                "new_string": "Hello, Cersei!"
            }), &ctx).await;
        }
        let avg_us = start.elapsed().as_micros() / iters as u128;
        check!(&format!("Edit avg {}μs (< 500μs target)", avg_us), avg_us < 500);

        let glob_tool = tools.iter().find(|t| t.name() == "Glob").unwrap();
        let start = std::time::Instant::now();
        for _ in 0..iters {
            glob_tool.execute(serde_json::json!({
                "pattern": "**/*.rs",
                "path": tmp.path().display().to_string()
            }), &ctx).await;
        }
        let avg_us = start.elapsed().as_micros() / iters as u128;
        check!(&format!("Glob avg {}μs (< 500μs target)", avg_us), avg_us < 500);
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

    if failed > 0 {
        std::process::exit(1);
    }
}
