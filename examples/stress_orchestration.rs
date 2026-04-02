//! # Phase 3 Stress Test — Sub-Agent Orchestration
//!
//! Tests AgentTool (sub-agent spawning), coordinator mode, task system,
//! and message passing between agents.
//!
//! ```bash
//! cargo run --example phase3_stress_test --release
//! ```

use cersei::prelude::*;
use cersei::provider::{CompletionStream, ProviderCapabilities};
use std::sync::Arc;
use tokio::sync::mpsc;

// ── Echo provider for testing ────────────────────────────────────────────────

struct EchoProvider;

#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &str { "echo" }
    fn context_window(&self, _: &str) -> u64 { 4096 }
    fn capabilities(&self, _: &str) -> ProviderCapabilities {
        ProviderCapabilities { streaming: true, tool_use: false, ..Default::default() }
    }
    async fn complete(&self, req: cersei::provider::CompletionRequest) -> cersei_types::Result<CompletionStream> {
        let prompt = req.messages.last().and_then(|m| m.get_text()).unwrap_or("").to_string();
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::MessageStart { id: "1".into(), model: "echo".into() }).await;
            let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "text".into(), id: None, name: None }).await;
            let _ = tx.send(StreamEvent::TextDelta { index: 0, text: format!("[Echo] {}", &prompt[..prompt.len().min(100)]) }).await;
            let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;
            let _ = tx.send(StreamEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: Some(Usage { input_tokens: 10, output_tokens: 5, ..Default::default() }),
            }).await;
            let _ = tx.send(StreamEvent::MessageStop).await;
        });
        Ok(CompletionStream::new(rx))
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
            if $cond { passed += 1; println!("  \x1b[32m✓\x1b[0m {}", $name); }
            else { failed += 1; println!("  \x1b[31m✗\x1b[0m {}", $name); }
        };
    }

    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║  Phase 3 — Sub-Agent Orchestration Stress Test   ║");
    println!("╚══════════════════════════════════════════════════╝\n");

    let ctx = ToolContext {
        working_dir: std::env::temp_dir(),
        session_id: "phase3".into(),
        permissions: Arc::new(AllowAll),
        cost_tracker: Arc::new(CostTracker::new()),
        mcp_manager: None,
        extensions: Extensions::default(),
    };

    // ── 1. AgentTool ─────────────────────────────────────────────────────
    println!("  1. AgentTool (Sub-Agent Spawning)");
    println!("  ─────────────────────────────────");
    {
        use cersei_agent::agent_tool::AgentTool;

        let agent_tool = AgentTool::new(
            || Box::new(EchoProvider),
            cersei::tools::filesystem(),
        );

        // Basic spawn
        let r = agent_tool.execute(serde_json::json!({
            "description": "test sub-agent",
            "prompt": "Hello from parent agent"
        }), &ctx).await;
        check!("Sub-agent spawns and returns", !r.is_error);
        check!("Sub-agent echoes prompt", r.content.contains("Echo"));
        check!("Sub-agent has metadata", r.metadata.is_some());

        if let Some(meta) = &r.metadata {
            check!("Metadata has turns", meta["turns"].is_number());
            check!("Metadata has tool_calls", meta["tool_calls"].is_number());
        }

        // With custom system prompt
        let r = agent_tool.execute(serde_json::json!({
            "description": "custom system",
            "prompt": "Do something",
            "system_prompt": "You are a Rust expert.",
            "max_turns": 3
        }), &ctx).await;
        check!("Custom system prompt works", !r.is_error);

        // Multiple sub-agents (parallel)
        let a1 = agent_tool.execute(serde_json::json!({
            "description": "worker 1", "prompt": "Task A"
        }), &ctx);
        let a2 = agent_tool.execute(serde_json::json!({
            "description": "worker 2", "prompt": "Task B"
        }), &ctx);
        let (r1, r2) = tokio::join!(a1, a2);
        check!("Parallel sub-agents both complete", !r1.is_error && !r2.is_error);
        check!("Worker 1 got Task A", r1.content.contains("Task A"));
        check!("Worker 2 got Task B", r2.content.contains("Task B"));
        println!();
    }

    // ── 2. Coordinator Mode ──────────────────────────────────────────────
    println!("  2. Coordinator Mode");
    println!("  ───────────────────");
    {
        use cersei_agent::coordinator::*;

        // Tool filtering
        let all_tools = cersei::tools::all();
        let all_count = all_tools.len();

        let worker_tools = filter_tools_for_mode(cersei::tools::all(), AgentMode::Worker);
        let coord_tools = filter_tools_for_mode(cersei::tools::all(), AgentMode::Coordinator);

        check!("Workers have fewer tools", worker_tools.len() < all_count);
        check!("Coordinator keeps all tools", coord_tools.len() == all_count);

        // Workers can't use SendMessage
        let has_send = worker_tools.iter().any(|t| t.name() == "SendMessage");
        check!("Workers can't SendMessage", !has_send);

        // Workers can't use TaskStop
        let has_stop = worker_tools.iter().any(|t| t.name() == "TaskStop");
        check!("Workers can't TaskStop", !has_stop);

        // Workers still have core tools
        let has_bash = worker_tools.iter().any(|t| t.name() == "Bash");
        let has_read = worker_tools.iter().any(|t| t.name() == "Read");
        check!("Workers have Bash", has_bash);
        check!("Workers have Read", has_read);

        // Coordinator prompt
        let prompt = coordinator_system_prompt();
        check!("Coordinator prompt mentions orchestrator", prompt.contains("orchestrator"));

        // Context listing
        let ctx_text = coordinator_context(&all_tools);
        check!("Context lists available tools", ctx_text.contains("Read") && ctx_text.contains("Bash"));
        println!();
    }

    // ── 3. Task System ───────────────────────────────────────────────────
    println!("  3. Task System (6 tools)");
    println!("  ────────────────────────");
    {
        cersei_tools::tasks::clear_tasks();

        // Create
        let create = cersei_tools::tasks::TaskCreateTool;
        let r = create.execute(serde_json::json!({"description": "Deploy staging"}), &ctx).await;
        check!("TaskCreate succeeds", !r.is_error);
        let id = r.content.split('\'').nth(1).unwrap().to_string();

        // List
        let list = cersei_tools::tasks::TaskListTool;
        let r = list.execute(serde_json::json!({}), &ctx).await;
        check!("TaskList shows task", r.content.contains("Deploy staging"));

        // Update to running
        let update = cersei_tools::tasks::TaskUpdateTool;
        update.execute(serde_json::json!({"id": &id, "status": "running"}), &ctx).await;
        let task = cersei_tools::tasks::get_task(&id).unwrap();
        check!("TaskUpdate sets running", task.status == cersei_tools::tasks::TaskStatus::Running);

        // Complete with output
        update.execute(serde_json::json!({
            "id": &id, "status": "completed", "output": "Deployed to staging-v42"
        }), &ctx).await;
        let task = cersei_tools::tasks::get_task(&id).unwrap();
        check!("Task completed with output", task.status == cersei_tools::tasks::TaskStatus::Completed);

        // Get output
        let output = cersei_tools::tasks::TaskOutputTool;
        let r = output.execute(serde_json::json!({"id": &id}), &ctx).await;
        check!("TaskOutput returns result", r.content.contains("staging-v42"));

        // Get status
        let get = cersei_tools::tasks::TaskGetTool;
        let r = get.execute(serde_json::json!({"id": &id}), &ctx).await;
        check!("TaskGet shows completed", r.content.contains("Completed"));

        // Create second task and stop it
        create.execute(serde_json::json!({"description": "Run migrations"}), &ctx).await;
        let tasks = cersei_tools::tasks::list_tasks();
        let second_id = tasks.iter().find(|t| t.description == "Run migrations").unwrap().id.clone();
        let stop = cersei_tools::tasks::TaskStopTool;
        stop.execute(serde_json::json!({"id": &second_id}), &ctx).await;
        let task = cersei_tools::tasks::get_task(&second_id).unwrap();
        check!("TaskStop stops task", task.status == cersei_tools::tasks::TaskStatus::Stopped);
        println!();
    }

    // ── 4. Message Passing ───────────────────────────────────────────────
    println!("  4. Inter-Agent Message Passing");
    println!("  ──────────────────────────────");
    {
        let send = cersei_tools::send_message::SendMessageTool;

        // Agent A sends to Agent B
        let ctx_a = ToolContext { session_id: "agent-alpha".into(), ..ctx.clone() };
        send.execute(serde_json::json!({"to": "agent-beta", "content": "Found 3 bugs"}), &ctx_a).await;
        send.execute(serde_json::json!({"to": "agent-beta", "content": "All fixed now"}), &ctx_a).await;

        // Agent B receives
        let msgs = cersei_tools::send_message::peek_inbox("agent-beta");
        check!("2 messages queued", msgs.len() == 2);
        check!("Messages from alpha", msgs.iter().all(|m| m.from == "agent-alpha"));

        // Drain
        let drained = cersei_tools::send_message::drain_inbox("agent-beta");
        check!("Drain returns all messages", drained.len() == 2);
        check!("Inbox empty after drain", cersei_tools::send_message::peek_inbox("agent-beta").is_empty());

        // Cross-session triggers
        let trigger = cersei_tools::remote_trigger::RemoteTriggerTool;
        trigger.execute(serde_json::json!({
            "target_session": "deploy-agent",
            "event_type": "tests_passed",
            "payload": {"suite": "integration", "passed": 99}
        }), &ctx_a).await;
        let events = cersei_tools::remote_trigger::drain_triggers("deploy-agent");
        check!("Trigger delivered", events.len() == 1);
        check!("Trigger has payload", events[0].payload["passed"] == 99);
        println!();
    }

    // ── 5. Full orchestration scenario ───────────────────────────────────
    println!("  5. Full Orchestration Scenario");
    println!("  ──────────────────────────────");
    {
        use cersei_agent::agent_tool::AgentTool;
        cersei_tools::tasks::clear_tasks();

        // Simulate coordinator creating tasks and spawning workers
        let create = cersei_tools::tasks::TaskCreateTool;
        create.execute(serde_json::json!({"description": "Lint check"}), &ctx).await;
        create.execute(serde_json::json!({"description": "Test suite"}), &ctx).await;
        create.execute(serde_json::json!({"description": "Build release"}), &ctx).await;

        let tasks = cersei_tools::tasks::list_tasks();
        check!("3 tasks created", tasks.len() >= 3);

        // Spawn 3 parallel workers
        let agent_tool = AgentTool::new(|| Box::new(EchoProvider), cersei::tools::coding());
        let mut handles = Vec::new();
        for task in &tasks {
            let desc = task.description.clone();
            let id = task.id.clone();
            let at = AgentTool::new(|| Box::new(EchoProvider), cersei::tools::coding());
            let ctx_clone = ctx.clone();
            handles.push(tokio::spawn(async move {
                let r = at.execute(serde_json::json!({
                    "description": desc.clone(),
                    "prompt": format!("Execute: {}", desc)
                }), &ctx_clone).await;
                (id, r)
            }));
        }

        let results: Vec<_> = futures::future::join_all(handles).await;
        let all_ok = results.iter().all(|r| r.as_ref().map(|(_, r)| !r.is_error).unwrap_or(false));
        check!("All 3 workers completed", all_ok);

        // Mark tasks complete
        let update = cersei_tools::tasks::TaskUpdateTool;
        for (id, result) in results.iter().filter_map(|r| r.as_ref().ok()) {
            update.execute(serde_json::json!({
                "id": id,
                "status": "completed",
                "output": &result.content
            }), &ctx).await;
        }

        let completed = cersei_tools::tasks::list_tasks()
            .iter()
            .filter(|t| t.status == cersei_tools::tasks::TaskStatus::Completed)
            .count();
        check!(&format!("{}/3 tasks completed", completed), completed >= 3);
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
